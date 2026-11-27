//! Snapshot the optional store trace in command order. The guest-memory
//! publication path still uses its original declarations and comparisons.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use victory_shader::emit::{
    RAW_STORE_PROBE_BASE, RAW_STORE_PROBE_CAPACITY, RAW_STORE_PROBE_DWORDS,
    RAW_STORE_PROBE_EVENT_DWORDS, RAW_STORE_PROBE_HASH, SLOT_FAULTS, raw_store_probe_enabled,
};
use windows::Win32::Graphics::Direct3D12::*;

use crate::host::{HostList, host, transition};
use crate::pipeline::host::HostPipeline;

const OFFSET: u64 = RAW_STORE_PROBE_BASE as u64 * 4;
const BYTES: u64 = RAW_STORE_PROBE_DWORDS as u64 * 4;
const HEADER_BYTES: usize = 8;
const ADDRESS_MASK: u64 = (1 << 40) - 1;

struct Owner {
    start: u64,
    len: u64,
    bias: u64,
    carded: bool,
}

pub(crate) fn after_dispatch(list: &HostList, pipeline: &HostPipeline) {
    if !raw_store_probe_enabled() {
        return;
    }
    let Some(stage) = pipeline.stages.first().filter(|stage| stage.shader.hash == RAW_STORE_PROBE_HASH) else {
        return;
    };
    if !stage.translated().host.root_uavs.contains(&SLOT_FAULTS) {
        tracing::warn!(shader = RAW_STORE_PROBE_HASH, "raw store trace UAV unavailable");
        return;
    }
    let frame = crate::queue::presented() + 1;
    let (faults, owners) = {
        let guard = super::MEMORY.lock().unwrap_or_else(|e| e.into_inner());
        let Some(memory) = guard.as_ref() else { return };
        // Keep the slot identity recorded with this dispatch. Publication can
        // run behind other recordings or remaps, and cannot take this mutex.
        let owners: Vec<_> = memory.copies.iter().map(|copy| {
            let (start, len) = copy.span();
            Owner { start, len, bias: copy.offset(start), carded: copy.carded() }
        }).collect();
        (memory.faults.0.clone(), owners)
    };
    let Some(readback) = snapshot_and_reset(list, &faults) else {
        tracing::warn!(shader = RAW_STORE_PROBE_HASH, frame, "raw store trace readback unavailable");
        return;
    };
    list.defer_after(move || {
        let Some(trace) = read_trace(&readback) else {
            tracing::warn!(shader = RAW_STORE_PROBE_HASH, frame, "raw store trace map failed");
            return;
        };
        report(frame, trace, &owners);
    });
}

fn snapshot_and_reset(list: &HostList, faults: &ID3D12Resource) -> Option<ID3D12Resource> {
    let host = host()?;
    static ZERO_HEADER: OnceLock<Option<ID3D12Resource>> = OnceLock::new();
    let zero = ZERO_HEADER.get_or_init(|| {
        let (buffer, mapped) = host.upload_buffer(HEADER_BYTES as u64)?;
        // SAFETY: a private, fresh allocation that is read-only after initialization.
        unsafe { mapped.write_bytes(0, HEADER_BYTES) };
        Some(buffer)
    }).as_ref()?;
    let readback = host.read_back_buffer(BYTES);
    let uav = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
    let source = D3D12_RESOURCE_STATE_COPY_SOURCE;
    let dest = D3D12_RESOURCE_STATE_COPY_DEST;
    // SAFETY: the instrumented dispatch used this buffer as a UAV. Copies and
    // reset occur in the same list after that dispatch. Only the two trace
    // header words are reset, leaving the shared fault directory untouched.
    unsafe {
        if let Some(readback) = &readback {
            list.list.ResourceBarrier(&[transition(faults, uav, source)]);
            list.list.CopyBufferRegion(readback, 0, faults, OFFSET, BYTES);
            list.list.ResourceBarrier(&[transition(faults, source, dest)]);
            list.keep(readback);
        } else {
            list.list.ResourceBarrier(&[transition(faults, uav, dest)]);
        }
        list.list.CopyBufferRegion(faults, OFFSET, zero, 0, HEADER_BYTES as u64);
        list.list.ResourceBarrier(&[transition(faults, dest, D3D12_RESOURCE_STATE_COMMON)]);
    }
    list.keep(faults);
    list.keep(zero);
    readback
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Store {
    address: u64,
    width: u32,
    slot: u32,
    host_byte: u32,
}

struct Trace {
    attempted: u32,
    overflow: bool,
    stores: Vec<Store>,
}

fn decode(words: &[u32; RAW_STORE_PROBE_DWORDS as usize]) -> Trace {
    let attempted = words[0];
    let stored = attempted.min(RAW_STORE_PROBE_CAPACITY) as usize;
    let stride = RAW_STORE_PROBE_EVENT_DWORDS as usize;
    let stores = words[2..2 + stored * stride].chunks_exact(stride).map(|entry| Store {
        address: (u64::from(entry[0]) | (u64::from(entry[1]) << 32)) & ADDRESS_MASK,
        width: entry[2],
        slot: entry[3],
        host_byte: entry[4],
    }).collect();
    Trace { attempted, overflow: words[1] != 0 || attempted > RAW_STORE_PROBE_CAPACITY, stores }
}

fn read_trace(readback: &ID3D12Resource) -> Option<Trace> {
    let mut data = std::ptr::null_mut();
    let range = D3D12_RANGE { Begin: 0, End: BYTES as usize };
    // SAFETY: the complete snapshot holds this array, and its list has finished.
    unsafe {
        readback.Map(0, Some(&range), Some(&mut data)).ok()?;
        let trace = data.cast::<[u32; RAW_STORE_PROBE_DWORDS as usize]>().as_ref().map(decode);
        readback.Unmap(0, Some(&D3D12_RANGE::default()));
        trace
    }
}

fn report(frame: u64, trace: Trace, owners: &[Owner]) {
    let mut ranges = Vec::with_capacity(trace.stores.len());
    let mut pages = BTreeSet::new();
    let mut slots = BTreeSet::new();
    let mut invalid = 0;
    let mut owner_mismatches = 0;
    let mut directory_crossings = 0;
    let mut card_store_events = 0;
    for Store { address, width, slot, host_byte } in trace.stores {
        let end = address + u64::from(width);
        if address % 4 != 0 || !matches!(width, 4 | 8 | 12 | 16) || slot == 0 || end > ADDRESS_MASK + 1 {
            invalid += 1;
            continue;
        }
        let owner = owners.get(slot as usize - 1).filter(|owner| {
            address.checked_sub(owner.start).is_some_and(|offset| {
                offset + u64::from(width) <= owner.len && offset + owner.bias == u64::from(host_byte)
            })
        });
        owner_mismatches += usize::from(owner.is_none());
        card_store_events += usize::from(owner.is_some_and(|owner| owner.carded));
        directory_crossings += usize::from(address / super::PAGE != (end - 1) / super::PAGE);
        slots.insert(slot);
        ranges.push((address, end));
        pages.extend(address / 4096..=(end - 1) / 4096);
    }
    ranges.sort_unstable();
    let mut total = 0;
    let mut previous_end = 0;
    for (start, end) in ranges {
        total += end.saturating_sub(start.max(previous_end));
        previous_end = previous_end.max(end);
    }
    tracing::info!(
        shader = RAW_STORE_PROBE_HASH, frame, attempted_stores = trace.attempted,
        overflow = trace.overflow, invalid, owner_mismatches, directory_crossings, card_store_events,
        exact_guest_bytes = total, dirty_pages_4k = pages.len(), owner_slots = slots.len(),
        "mapped raw stores after the GPU fence"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_keeps_records_and_resets_only_trace_header() {
        if std::env::var_os("VICTORY_RAW_STORE_READBACK_PROBE").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return;
        }
        let host = host().expect("D3D12 device for the opt-in trace readback probe");
        let (faults, mapped, _) = super::super::Memory::table(
            u64::from(RAW_STORE_PROBE_BASE + RAW_STORE_PROBE_DWORDS), "raw store test",
        ).unwrap();
        let original = [1, 0, 0x2345_fff0, 0x101, 16, 5, 0x20010];
        let expected = Store { address: 0x01_2345_fff0, width: 16, slot: 5, host_byte: 0x20010 };
        let prefix = RAW_STORE_PROBE_BASE as usize - 1;
        let list = host.new_list().unwrap();
        let mark_uav = || {
            // SAFETY: test-local buffer starts and is left in COMMON.
            unsafe { list.list.ResourceBarrier(&[transition(
                &faults, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]) };
        };
        // SAFETY: the buffer is mapped and private, with no GPU access yet.
        unsafe {
            mapped.add(prefix).write(0x1357_2468);
            mapped.add(prefix + 1).cast::<[u32; 7]>().write(original);
        }
        mark_uav();
        let first = snapshot_and_reset(&list, &faults).unwrap();
        mark_uav();
        let second = snapshot_and_reset(&list, &faults).unwrap();
        list.close();
        host.execute(&[&list]);
        let first = read_trace(&first).unwrap();
        assert_eq!(first.attempted, 1);
        assert!(!first.overflow);
        assert_eq!(first.stores, [expected]);
        assert_eq!(read_trace(&second).unwrap().attempted, 0);
        // SAFETY: the list completed, the mapped prefix and event are still live.
        assert_eq!(unsafe { mapped.add(prefix).read() }, 0x1357_2468);
        assert_eq!(unsafe { mapped.add(prefix + 3).read() }, original[2]);
    }
}
