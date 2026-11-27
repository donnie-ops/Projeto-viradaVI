//! Probe the GPU-to-CPU demand boundary with an isolated shadow and an
//! explicit CPU consumer. The retail runtime does not use this shadow.

use crate::host::{Host, compile, host, transition};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

const PAGE_BYTES: usize = 4096;
const PAGE_WORDS: usize = PAGE_BYTES / size_of::<u32>();
const PAGES: usize = 4;
const SEED: u32 = 0x3c72_1100;
const CPU_MARKER: u32 = 0x0bad_beef;

/// The readback and copy list are created only at the first CPU access to
/// stale data. Dirty pages have never been read from the GPU by this code.
struct DemandShadow<'a> {
    host: &'a Host,
    gpu: ID3D12Resource,
    cpu: Vec<u32>,
    dirty: [bool; PAGES],
    reads: Vec<(usize, usize)>,
    readback: Option<ID3D12Resource>,
}

impl<'a> DemandShadow<'a> {
    fn new(host: &'a Host) -> Self {
        Self {
            host,
            gpu: host.uav_buffer((PAGES * PAGE_BYTES) as u64).unwrap(),
            cpu: vec![CPU_MARKER; PAGES * PAGE_WORDS],
            dirty: [false; PAGES],
            reads: Vec::new(),
            readback: None,
        }
    }

    fn produce(&mut self) {
        let hlsl = r#"
            RWByteAddressBuffer data : register(u0);
            cbuffer Params : register(b0) { uint seed; };
            [RootSignature("UAV(u0), RootConstants(num32BitConstants=1,b0)")]
            [numthreads(64,1,1)] void main(uint3 id : SV_DispatchThreadID) {
                data.Store(id.x * 4, seed ^ (id.x * 3));
            }"#;
        let code = compile(hlsl, s!("main"), s!("cs_5_1")).unwrap();
        let list = self.host.new_list().unwrap();
        // SAFETY: the CPU objects and default-heap buffer outlive this
        // submission, and Dispatch writes exactly the buffer's words.
        let (root, pso): (ID3D12RootSignature, ID3D12PipelineState) = unsafe {
            let root = self.host.device.CreateRootSignature(0, &code).unwrap();
            let pso = self.host.device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            }).unwrap();
            list.list.ResourceBarrier(&[transition(&self.gpu, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
            list.list.SetComputeRootSignature(&root);
            list.list.SetPipelineState(&pso);
            list.list.SetComputeRootUnorderedAccessView(0, self.gpu.GetGPUVirtualAddress());
            list.list.SetComputeRoot32BitConstant(1, SEED, 0);
            list.list.Dispatch((PAGES * PAGE_WORDS / 64) as u32, 1, 1);
            list.list.ResourceBarrier(&[transition(&self.gpu, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON)]);
            (root, pso)
        };
        list.close();
        self.host.execute(&[&list]);
        drop((root, pso));
        self.dirty.fill(true);
    }

    fn materialize(&mut self, page: usize) {
        if !self.dirty[page] { return }
        let offset = page * PAGE_BYTES;
        let readback = self.readback.get_or_insert_with(|| self.host.read_back_buffer(PAGE_BYTES as u64).unwrap());
        let list = self.host.new_list().unwrap();
        // SAFETY: the requested page is inside the live GPU buffer and the
        // readback holds exactly one page. Execute waits for this copy.
        unsafe {
            list.list.ResourceBarrier(&[transition(&self.gpu, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
            list.list.CopyBufferRegion(&*readback, 0, &self.gpu, offset as u64, PAGE_BYTES as u64);
            list.list.ResourceBarrier(&[transition(&self.gpu, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]);
        }
        list.close();
        self.host.execute(&[&list]);
        let mut data = std::ptr::null_mut();
        // SAFETY: the copy fence has completed and both ranges hold one page.
        unsafe {
            readback.Map(0, Some(&D3D12_RANGE { Begin: 0, End: PAGE_BYTES }), Some(&mut data)).unwrap();
            let dst = self.cpu[page * PAGE_WORDS..].as_mut_ptr();
            std::ptr::copy_nonoverlapping(data.cast::<u32>(), dst, PAGE_WORDS);
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
        }
        self.reads.push((offset, PAGE_BYTES));
        self.dirty[page] = false;
    }

    fn cpu_read(&mut self, word: usize) -> u32 {
        self.materialize(word / PAGE_WORDS);
        self.cpu[word]
    }

    fn cpu_write_page(&mut self, page: usize, value: u32) {
        self.cpu[page * PAGE_WORDS..(page + 1) * PAGE_WORDS].fill(value);
        self.dirty[page] = false;
    }

    fn cpu_write_word(&mut self, word: usize, value: u32) {
        self.materialize(word / PAGE_WORDS);
        self.cpu[word] = value;
    }
}

/// Enable with VICTORY_DEFERRED_READBACK_PROBE=1. WARP and hardware use the
/// same copy/fence path; neither test changes the game's readback behavior.
#[test]
fn first_cpu_demand_reads_only_its_gpu_dirty_page() {
    if std::env::var_os("VICTORY_DEFERRED_READBACK_PROBE").is_none_or(|value| value != "1") { return }
    let host = host().expect("D3D12 device for the opt-in deferred-readback probe");
    let mut shadow = DemandShadow::new(host);
    let expected = |word: usize| SEED ^ ((word as u32) * 3);
    shadow.produce();
    assert!(shadow.readback.is_none());
    assert!(shadow.reads.is_empty());
    assert!(shadow.cpu.iter().all(|&word| word == CPU_MARKER));

    assert_eq!(shadow.cpu_read(PAGE_WORDS + 17), expected(PAGE_WORDS + 17));
    assert_eq!(shadow.reads, [(PAGE_BYTES, PAGE_BYTES)]);
    assert!(shadow.cpu[PAGE_WORDS..2 * PAGE_WORDS].iter().copied().eq((PAGE_WORDS..2 * PAGE_WORDS).map(expected)));
    assert_eq!(shadow.cpu_read(PAGE_WORDS + 25), expected(PAGE_WORDS + 25));
    assert_eq!(shadow.reads.len(), 1);
    assert_eq!(shadow.dirty, [true, false, true, true]);

    shadow.cpu_write_page(0, CPU_MARKER);
    assert_eq!(shadow.cpu_read(42), CPU_MARKER);
    assert_eq!(shadow.reads.len(), 1, "a full CPU overwrite owns the page without reading it back");

    let partial = 2 * PAGE_WORDS + 3;
    shadow.cpu_write_word(partial, CPU_MARKER);
    assert_eq!(shadow.cpu_read(partial), CPU_MARKER);
    assert_eq!(shadow.cpu_read(partial - 1), expected(partial - 1));
    assert_eq!(shadow.cpu_read(partial + 1), expected(partial + 1));
    assert_eq!(shadow.reads, [(PAGE_BYTES, PAGE_BYTES), (2 * PAGE_BYTES, PAGE_BYTES)]);
    assert!(shadow.dirty[PAGES - 1], "an unread page stays GPU-authoritative");
}
