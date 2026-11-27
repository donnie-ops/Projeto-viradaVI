//! Classify the upload fills that block submissions on earlier writes.
//! This changes no copy, descriptor, or fence.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Late, Line, line, pending_bottom_writes, touches};

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("VICTORY_CARD_STEPS_PROBE").is_some_and(|v| v == "1"))
}

const OWNERS: [&str; 8] = [
    "device-local copy",
    "system-memory copy",
    "more than one copy",
    "copy covers only part",
    "CPU publication",
    "untracked completion",
    "unknown",
    "already retired",
];

#[derive(Clone, Copy)]
enum Owner {
    DeviceLocal,
    SystemMemory,
    Multiple,
    Partial,
    Cpu,
    Opaque,
    Unknown,
    Retired,
}

/// The queued producer's declared backing. With a relay landing in progress,
/// another producer may already have been removed from the inspectable queue.
fn queued_owner(line: &Line, start: usize, end: usize) -> Owner {
    if line.written.opaque_completion {
        return Owner::Opaque;
    }
    if touches(&line.written.textures, start, end)
        || pending_bottom_writes(line)
            .any(|(at, _, len)| at < end && start < at.saturating_add(len))
    {
        return Owner::Cpu;
    }
    if !line.written.touches(start, end) {
        return Owner::Retired;
    }
    let mut owner: Option<&crate::shadow::Shadow> = None;
    for item in &line.line {
        let Late::Run(flight) = item else { continue };
        for (shadow, ranges) in &flight.back {
            if ranges.iter().any(|&(lo, hi)| lo < end && start < hi) {
                if owner.is_some_and(|prior| prior.id() != shadow.id()) {
                    return Owner::Multiple;
                }
                owner = Some(shadow);
            }
        }
    }
    let Some(copy) = owner else {
        return Owner::Unknown;
    };
    let (base, len) = copy.span();
    if (start as u64) < base || end as u64 > base.saturating_add(len) {
        Owner::Partial
    } else if copy.carded() {
        Owner::DeviceLocal
    } else {
        Owner::SystemMemory
    }
}

// By fill kind, relay landing state, and queued backing.
static COUNTS: [[[AtomicU64; OWNERS.len()]; 2]; 2] =
    [const { [const { [const { AtomicU64::new(0) }; OWNERS.len()] }; 2] }; 2];
static SAMPLES: AtomicU64 = AtomicU64::new(0);

pub(super) fn note(start: usize, bytes: usize, table_slot: bool) {
    let Some(end) = start.checked_add(bytes) else {
        return;
    };
    let (owner, relay_landing) = {
        let line = line();
        (queued_owner(&line, start, end), line.settling)
    };
    COUNTS[usize::from(table_slot)][usize::from(relay_landing)][owner as usize]
        .fetch_add(1, Ordering::Relaxed);
    let sample = SAMPLES.fetch_add(1, Ordering::Relaxed) + 1;
    if sample.is_power_of_two() {
        tracing::info!(
            sample,
            source = format_args!("{start:#x}"),
            bytes,
            table_slot,
            relay_landing,
            queued_owner = OWNERS[owner as usize],
            "blocked upload fill"
        );
    }
}

pub(super) fn report(frames: f64) {
    if !enabled() {
        return;
    }
    for (kind, by_landing) in COUNTS.iter().enumerate() {
        for (landing, counts) in by_landing.iter().enumerate() {
            for (owner, count) in counts.iter().enumerate() {
                let fills = count.swap(0, Ordering::Relaxed);
                if fills != 0 {
                    tracing::info!(
                        table_slot = kind != 0,
                        relay_landing = landing != 0,
                        queued_owner = OWNERS[owner],
                        fills,
                        fills_per_frame = fills as f64 / frames,
                        "blocked upload fill counters"
                    );
                }
            }
        }
    }
}
