//! Suballocate short-lived texture staging uploads from persistently mapped buffers.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

use crate::host::Host;

const CHUNK_BYTES: u64 = 2 << 20;
const MIN_SHIFT: u32 = 12;
const MAX_SHIFT: u32 = 19;
const CLASSES: usize = (MAX_SHIFT - MIN_SHIFT + 1) as usize;

struct Chunk {
    buffer: ID3D12Resource,
    address: usize,
}

struct Slots {
    chunk: Arc<Chunk>,
    free: Vec<u32>,
}

static POOL: Mutex<[Vec<Slots>; CLASSES]> = Mutex::new([const { Vec::new() }; CLASSES]);
static REUSE: AtomicU64 = AtomicU64::new(0);
static CREATED: AtomicU64 = AtomicU64::new(0);

/// The lifetime of a staging allocation, including every command list that
/// still holds its `Staging`. Its bytes cannot be recycled until that ends.
pub(super) struct Upload {
    chunk: Arc<Chunk>,
    slot: Option<(usize, u32)>,
    pub(super) offset: u64,
}

impl Upload {
    pub(super) fn new(host: &Host, bytes: u64) -> Option<Self> {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var_os("VICTORY_TEXTURE_UPLOAD_POOL").is_none_or(|v| v != "0")
        });
        if enabled && (1..=1 << MAX_SHIFT).contains(&bytes) {
            let size = bytes.max(1 << MIN_SHIFT).next_power_of_two();
            let class = (size.ilog2() - MIN_SHIFT) as usize;
            if let Some(upload) = Self::pooled(host, class, size) {
                // SAFETY: the pool reserved this slot exclusively and it has
                // no in-flight readers. A partial or failed refresh can leave
                // some bytes untouched, so start with the same zeros as a new
                // committed upload buffer.
                unsafe { upload.data().write_bytes(0, bytes as usize) };
                return Some(upload);
            }
        }
        let (buffer, data) = host.upload_buffer(bytes)?;
        Some(Self {
            chunk: Arc::new(Chunk {
                buffer,
                address: data as usize,
            }),
            slot: None,
            offset: 0,
        })
    }

    fn pooled(host: &Host, class: usize, size: u64) -> Option<Self> {
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        let slots = &mut pool[class];
        if let Some(page) = slots.iter_mut().find(|page| !page.free.is_empty()) {
            let index = page.free.pop()?;
            REUSE.fetch_add(1, Relaxed);
            return Some(Self::from_slot(page, class, index, size));
        }

        let (buffer, data) = host.upload_buffer(CHUNK_BYTES)?;
        crate::host::name(&buffer, || "texture staging pool".into());
        let page = Slots {
            chunk: Arc::new(Chunk {
                buffer,
                address: data as usize,
            }),
            free: (1..(CHUNK_BYTES / size) as u32).rev().collect(),
        };
        let upload = Self::from_slot(&page, class, 0, size);
        slots.push(page);
        CREATED.fetch_add(1, Relaxed);
        Some(upload)
    }

    fn from_slot(page: &Slots, class: usize, index: u32, size: u64) -> Self {
        Self {
            chunk: page.chunk.clone(),
            slot: Some((class, index)),
            offset: u64::from(index) * size,
        }
    }

    pub(super) fn data(&self) -> *mut u8 {
        (self.chunk.address + self.offset as usize) as *mut u8
    }

    pub(super) fn buffer(&self) -> &ID3D12Resource {
        &self.chunk.buffer
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let Some((class, index)) = self.slot else {
            return;
        };
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        let slots = &mut pool[class];
        let page_index = slots
            .iter()
            .position(|page| Arc::ptr_eq(&page.chunk, &self.chunk))
            .expect("a leased texture staging page must be in its size class");
        let capacity = (CHUNK_BYTES >> (MIN_SHIFT as usize + class)) as usize;
        slots[page_index].free.push(index);
        if slots[page_index].free.len() == capacity
            && slots
                .iter()
                .enumerate()
                .any(|(i, page)| i != page_index && page.free.len() == capacity)
        {
            // Keep at most one idle chunk per size class. This Upload still
            // holds the chunk, so final COM release occurs outside the lock.
            slots.swap_remove(page_index);
        }
    }
}

#[cfg(feature = "diagnostics")]
pub(super) fn totals() -> (u64, u64) {
    (CREATED.load(Relaxed), REUSE.load(Relaxed))
}
