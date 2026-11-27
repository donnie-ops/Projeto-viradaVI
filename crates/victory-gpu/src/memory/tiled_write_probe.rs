//! Measures the existing hash path and explicit GPU write masks against the
//! same writes into a reserved buffer. The probe uses its own small heap.

use super::{CARD_COMPARE, Host, compile, host, transition};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::core::s;

const TILE: u64 = 64 << 10;
const TILES: u32 = 512;
const BYTES: u64 = TILE * TILES as u64;
const MASK_BYTES: u64 = 4 * TILES as u64;

struct Compute {
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
}

impl Compute {
    fn new(host: &Host, source: &str) -> Self {
        let bytecode =
            compile(source, s!("main"), s!("cs_5_1")).expect("compile synthetic compute shader");
        // SAFETY: the compiled shader embeds its root signature.
        unsafe {
            let root = host
                .device
                .CreateRootSignature::<ID3D12RootSignature>(0, &bytecode)
                .unwrap();
            let pipeline = host
                .device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: bytecode.as_ptr().cast(),
                        BytecodeLength: bytecode.len(),
                    },
                    ..Default::default()
                })
                .unwrap();
            Self { root, pipeline }
        }
    }

    fn bind(&self, list: &ID3D12GraphicsCommandList) {
        // SAFETY: a recording list and a pipeline made with this signature.
        unsafe {
            list.SetComputeRootSignature(&self.root);
            list.SetPipelineState(&self.pipeline);
        }
    }
}

fn reserve(host: &Host) -> windows::core::Result<(ID3D12Resource, ID3D12Heap)> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: BYTES + 8 * TILE, // Leave the last eight tiles unmapped.
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let heap_desc = D3D12_HEAP_DESC {
        SizeInBytes: BYTES,
        Properties: D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        },
        Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS,
        ..Default::default()
    };
    let (mut buffer, mut heap): (Option<ID3D12Resource>, Option<ID3D12Heap>) = (None, None);
    // SAFETY: the heap backs only the first TILES tiles. The queue orders
    // their mappings before the copies and shader dispatches below.
    unsafe {
        host.device.CreateReservedResource(
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut buffer,
        )?;
        host.device.CreateHeap(&heap_desc, &mut heap)?;
        let buffer = buffer.unwrap();
        let heap = heap.unwrap();
        let region = D3D12_TILE_REGION_SIZE {
            NumTiles: TILES,
            ..Default::default()
        };
        host.queue.UpdateTileMappings(
            &buffer,
            1,
            Some(&D3D12_TILED_RESOURCE_COORDINATE::default()),
            Some(&region),
            &heap,
            1,
            Some(&D3D12_TILE_RANGE_FLAG_NONE),
            Some(&0),
            Some(&TILES),
            D3D12_TILE_MAPPING_FLAG_NONE,
        );
        Ok((buffer, heap))
    }
}

const WRITER: &str = r#"
    RWByteAddressBuffer arena : register(u0);
    RWByteAddressBuffer dirty : register(u1);
    cbuffer Args : register(b0) { uint step; };
    [RootSignature("RootConstants(num32BitConstants=1,b0), UAV(u0), UAV(u1)")]
    [numthreads(64,1,1)]
    void main(uint3 group : SV_GroupID, uint lane : SV_GroupIndex) {
        uint tile = group.x * step;
        uint page = lane / 8;
        uint at = tile * 65536 + page * 4096 + (lane % 8) * 16;
        arena.Store4(at, uint4(1, 0x502, 3, 0x700 + lane));
        #if TRACK_WRITES
        uint prior;
        dirty.InterlockedOr(tile * 4, 1 << page, prior);
        #endif
    }
"#;

fn uav_barrier(resource: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            // SAFETY: borrow the caller's resource for command recording.
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: unsafe { std::mem::transmute_copy(resource) },
            }),
        },
    }
}

/// This is a correctness probe with GPU timestamps, not a game benchmark.
#[test]
#[ignore = "run explicitly on the adapter being measured"]
fn reserved_gpu_buffer_write_masks_match_snapshot_compare() {
    let host = host().expect("D3D12 adapter");
    let (arena, _tiles) = match reserve(host) {
        Ok(resources) => resources,
        Err(error) if host.is_software() => {
            eprintln!("software adapter could not allocate a reserved buffer: {error}");
            return;
        }
        Err(error) => panic!("native adapter could not allocate a reserved buffer: {error}"),
    };
    let reference = Compute::new(host, CARD_COMPARE);
    let untracked = Compute::new(host, &format!("#define TRACK_WRITES 0\n{WRITER}"));
    let tracked = Compute::new(host, &format!("#define TRACK_WRITES 1\n{WRITER}"));
    let (zeros, zero_data) = host.upload_buffer(BYTES).unwrap();
    let hashes = host
        .uav_buffer(crate::shadow::card_hash_bytes(BYTES))
        .unwrap();
    let mask = host.uav_buffer(MASK_BYTES).unwrap();
    let readback = host.read_back_buffer(MASK_BYTES + 24).unwrap();
    let mut queries: Option<ID3D12QueryHeap> = None;
    // SAFETY: the buffers are live and the mapped allocation is BYTES long.
    let (arena_gpu, hashes_gpu, mask_gpu, hz) = unsafe {
        zero_data.write_bytes(0, BYTES as usize);
        host.device
            .CreateQueryHeap(
                &D3D12_QUERY_HEAP_DESC {
                    Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                    Count: 2,
                    NodeMask: 0,
                },
                &mut queries,
            )
            .unwrap();
        (
            arena.GetGPUVirtualAddress(),
            hashes.GetGPUVirtualAddress(),
            mask.GetGPUVirtualAddress(),
            host.queue.GetTimestampFrequency().unwrap(),
        )
    };
    let queries = queries.unwrap();
    let copy_dest = D3D12_RESOURCE_STATE_COPY_DEST;
    let copy_source = D3D12_RESOURCE_STATE_COPY_SOURCE;
    let uav = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
    let srv = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    let common = D3D12_RESOURCE_STATE_COMMON;

    let run = |step: u32, tracked_writes: bool| {
        let commands = host.new_list().unwrap();
        let list: &ID3D12GraphicsCommandList = &commands.list;
        let compare = |mode: u32| {
            reference.bind(list);
            let args = [0, 0, BYTES as u32, TILES, 0, mode];
            // SAFETY: full mapped prefix and an output dword per tile.
            unsafe {
                list.SetComputeRoot32BitConstants(0, 6, args.as_ptr().cast(), 0);
                list.SetComputeRootShaderResourceView(1, arena_gpu);
                list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                list.SetComputeRootUnorderedAccessView(3, mask_gpu);
                list.Dispatch(TILES, 1, 1);
            }
        };
        // SAFETY: each range fits its resource. No command touches the
        // reserved tail. States are common on entry and exit of every run.
        unsafe {
            list.ResourceBarrier(&[
                transition(&arena, common, copy_dest),
                transition(&hashes, common, uav),
            ]);
            list.CopyBufferRegion(&arena, 0, &zeros, 0, BYTES);
            list.EndQuery(&queries, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            list.ResourceBarrier(&[transition(&mask, common, copy_dest)]);
            list.CopyBufferRegion(&mask, 0, &zeros, 0, MASK_BYTES);
            list.ResourceBarrier(&[transition(&mask, copy_dest, uav)]);
            if tracked_writes {
                list.ResourceBarrier(&[transition(&arena, copy_dest, uav)]);
                tracked.bind(list);
            } else {
                list.ResourceBarrier(&[transition(&arena, copy_dest, srv)]);
                compare(0);
                list.ResourceBarrier(&[transition(&arena, srv, uav), uav_barrier(&hashes)]);
                untracked.bind(list);
            }
            list.SetComputeRoot32BitConstant(0, step, 0);
            list.SetComputeRootUnorderedAccessView(1, arena_gpu);
            list.SetComputeRootUnorderedAccessView(2, mask_gpu);
            list.Dispatch(TILES / step, 1, 1);
            list.ResourceBarrier(&[transition(&arena, uav, srv)]);
            if !tracked_writes {
                compare(1);
            }
            list.ResourceBarrier(&[
                transition(&mask, uav, copy_source),
                transition(&arena, srv, common),
                transition(&hashes, uav, common),
            ]);
            list.CopyBufferRegion(&readback, 0, &mask, 0, MASK_BYTES);
            list.ResourceBarrier(&[transition(&mask, copy_source, common)]);
            list.EndQuery(&queries, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(
                &queries,
                D3D12_QUERY_TYPE_TIMESTAMP,
                0,
                2,
                &readback,
                MASK_BYTES,
            );
        }
        commands.close();
        host.execute(&[&commands]);

        // SAFETY: host.execute waited for the queue; readback holds the
        // complete mask followed by two 8-byte timestamps.
        unsafe {
            let mut mapping = std::ptr::null_mut();
            readback.Map(0, None, Some(&mut mapping)).unwrap();
            let masks = std::slice::from_raw_parts(mapping.cast::<u32>(), TILES as usize).to_vec();
            let ticks = mapping
                .cast::<u8>()
                .add(MASK_BYTES as usize)
                .cast::<[u64; 2]>()
                .read();
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
            (masks, 1000.0 * (ticks[1] - ticks[0]) as f64 / hz as f64)
        }
    };

    for step in [16, 1] {
        let expected: Vec<u32> = (0..TILES)
            .map(|tile| if tile % step == 0 { 0xff } else { 0 })
            .collect();
        let (reference_mask, _) = run(step, false);
        let (write_mask, _) = run(step, true);
        assert_eq!(reference_mask, expected);
        assert_eq!(write_mask, expected);
        let mut samples = [Vec::new(), Vec::new()];
        for _ in 0..5 {
            samples[0].push(run(step, false).1);
            samples[1].push(run(step, true).1);
        }
        samples
            .iter_mut()
            .for_each(|sample| sample.sort_unstable_by(f64::total_cmp));
        eprintln!(
            "reserved buffer {} MiB, {}/{} tiles touched, GPU time median: snapshot compare {:.3} ms, write mask {:.3} ms",
            BYTES >> 20,
            TILES / step,
            TILES,
            samples[0][2],
            samples[1][2]
        );
    }
    // SAFETY: querying the live test device, after both sets of workloads.
    unsafe { host.device.GetDeviceRemovedReason().unwrap() };
}
