//! Place small read-only 2D textures on disjoint portions of 64 KiB heaps.
//! Offsets advance monotonically. No GPU accesses to different resources can
//! alias, including across lists or when descriptors outlive the title texture.

use std::collections::HashMap;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::GUID;

const PAGE: u64 = D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT as u64;
const ALIGN: u64 = D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT as u64;
const BACKING_HEAP: GUID = GUID::from_u128(0x23d43667_69e5_419c_91c1_93111c29e7ac);

#[derive(Default)]
pub(crate) struct SmallTextures {
    pool: Mutex<Pool>,
    #[cfg(feature = "diagnostics")]
    placed: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct Pool {
    current: Option<(ID3D12Heap, u64)>,
    #[cfg(feature = "diagnostics")]
    heaps_made: u64,
    // Layout is native-device-specific. Cache only the common shapes.
    sizes: HashMap<(i32, u64, u32, u16), Option<u64>>,
}

/// Only single-plane color and BC textures. A 4 KiB tile contains the given
/// number of blocks, and the most detailed mip may span at most 16 such tiles.
fn candidate(d: &D3D12_RESOURCE_DESC) -> bool {
    if d.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
        || d.Flags != D3D12_RESOURCE_FLAG_NONE
        || d.Layout != D3D12_TEXTURE_LAYOUT_UNKNOWN
        || d.DepthOrArraySize != 1
        || d.SampleDesc.Count != 1
        || d.SampleDesc.Quality != 0
        || d.Alignment != 0
    {
        return false;
    }
    let (tile_width, tile_height) = match d.Format.0 {
        1..=4 => (16, 16),                                      // 128-bit color
        9..=18 => (32, 16),                                     // 64-bit color
        23..=32 | 34..=38 | 41..=43 | 67 | 87..=93 => (32, 32), // 32-bit color
        48..=54 | 56..=59 => (64, 32),                          // 16-bit color
        60..=65 => (64, 64),                                    // 8-bit color
        70..=72 | 79..=81 => (128, 64),                         // 8-byte BC blocks
        73..=78 | 82..=84 | 94..=99 => (64, 64),                // 16-byte BC blocks
        _ => return false,
    };
    d.Width > 0
        && d.Height > 0
        && d.Width.div_ceil(tile_width) <= 16 / u64::from(d.Height.div_ceil(tile_height))
}

impl SmallTextures {
    #[cfg(feature = "diagnostics")]
    pub(crate) fn totals(&self) -> (u64, u64) {
        let pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        (
            self.placed.load(std::sync::atomic::Ordering::Relaxed),
            pool.heaps_made,
        )
    }

    pub(super) fn place(
        &self,
        device: &ID3D12Device,
        desc: &D3D12_RESOURCE_DESC,
        state: D3D12_RESOURCE_STATES,
    ) -> Option<ID3D12Resource> {
        if !candidate(desc) {
            return None;
        }
        let d = D3D12_RESOURCE_DESC {
            Alignment: ALIGN,
            ..*desc
        };
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        let shape = (d.Format.0, d.Width, d.Height, d.MipLevels);
        let size = match pool.sizes.get(&shape) {
            Some(size) => *size,
            None => {
                // SAFETY: query the complete native description, including every mip.
                let allocation = unsafe { device.GetResourceAllocationInfo(0, &[d]) };
                let size = (allocation.Alignment == ALIGN
                    && (ALIGN..PAGE).contains(&allocation.SizeInBytes)
                    && allocation.SizeInBytes.is_multiple_of(ALIGN))
                .then_some(allocation.SizeInBytes);
                if pool.sizes.len() < 256 {
                    pool.sizes.insert(shape, size);
                }
                size
            }
        }?;
        if pool
            .current
            .as_ref()
            .is_none_or(|(_, used)| used + size > PAGE)
        {
            let mut heap = None;
            let desc = D3D12_HEAP_DESC {
                SizeInBytes: PAGE,
                Alignment: PAGE,
                Properties: D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES,
            };
            // SAFETY: a standard aligned default heap and a typed out pointer.
            unsafe { device.CreateHeap(&desc, &mut heap).ok()? };
            pool.current = Some((heap?, 0));
            #[cfg(feature = "diagnostics")]
            {
                pool.heaps_made += 1;
            }
        }
        let (heap, used) = pool.current.as_mut()?;
        let placement = (heap.clone(), *used);
        *used += size;
        if *used == PAGE {
            pool.current = None;
        }
        // Reserve before releasing the lock. Native creation on different title
        // threads can proceed independently; a failed range is never reused.
        drop(pool);
        let (heap, offset) = placement;
        let mut texture: Option<ID3D12Resource> = None;
        // SAFETY: this range is aligned, fits this heap, and has never been used.
        unsafe {
            device
                .CreatePlacedResource(&heap, offset, &d, state, None, &mut texture)
                .ok()?
        };
        let texture = texture?;
        // Also retain the heap for independent COM references in descriptors and
        // in-flight command lists, through final release of the native texture.
        unsafe { texture.SetPrivateDataInterface(&BACKING_HEAP, &heap).ok()? };
        #[cfg(feature = "diagnostics")]
        self.placed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(texture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    fn desc(size: (u64, u32), format: DXGI_FORMAT, mips: u16) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: size.0,
            Height: size.1,
            DepthOrArraySize: 1,
            MipLevels: mips,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        }
    }

    #[test]
    fn small_texture_eligibility_excludes_writable_large_and_multisample_resources() {
        let d = desc((128, 128), DXGI_FORMAT_BC7_UNORM, 8);
        assert!(candidate(&d));
        for invalid in [
            D3D12_RESOURCE_DESC {
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                ..d
            },
            D3D12_RESOURCE_DESC {
                Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                ..d
            },
            D3D12_RESOURCE_DESC {
                DepthOrArraySize: 2,
                ..d
            },
            D3D12_RESOURCE_DESC {
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 4,
                    Quality: 0,
                },
                ..d
            },
            D3D12_RESOURCE_DESC {
                Width: 1025,
                Height: 65,
                ..d
            },
            D3D12_RESOURCE_DESC {
                Format: DXGI_FORMAT_NV12,
                ..d
            },
            D3D12_RESOURCE_DESC { Height: 0, ..d },
        ] {
            assert!(!candidate(&invalid));
        }
    }

    #[test]
    #[cfg(feature = "diagnostics")]
    fn small_placed_textures_share_backing_and_keep_distinct_bytes_after_pool_drop() {
        use std::collections::HashSet;
        use windows::core::Interface;

        if !crate::host::on_warp() {
            return;
        }
        let host = crate::host::host().expect("requested WARP device unavailable");
        assert!(host.is_software());
        let pool = SmallTextures::default();
        let mut textures = Vec::new();
        let mut heaps = HashSet::new();
        let mut committed_bytes = 0;
        // Common material texture sizes with both single- and multi-mip resources.
        for (d, count) in [
            (desc((16, 4), DXGI_FORMAT_R16G16B16A16_FLOAT, 1), 16),
            (desc((32, 32), DXGI_FORMAT_BC7_UNORM, 1), 5),
            (desc((128, 128), DXGI_FORMAT_BC7_UNORM, 8), 9),
            (desc((256, 256), DXGI_FORMAT_BC1_UNORM, 1), 17),
            (desc((256, 256), DXGI_FORMAT_BC4_UNORM, 1), 16),
        ] {
            for _ in 0..count {
                let t = pool
                    .place(&host.device, &d, D3D12_RESOURCE_STATE_COPY_DEST)
                    .unwrap();
                let mut backing: Option<ID3D12Heap> = None;
                let mut ptr_bytes = size_of::<Option<ID3D12Heap>>() as u32;
                unsafe {
                    // GetPrivateData AddRefs the interface for this temporary handle.
                    t.GetPrivateData(
                        &BACKING_HEAP,
                        &mut ptr_bytes,
                        Some((&raw mut backing).cast()),
                    )
                    .unwrap();
                    assert_eq!(t.GetDesc().MipLevels, d.MipLevels);
                    let backing = backing.unwrap();
                    assert_eq!(backing.GetDesc().SizeInBytes, PAGE);
                    heaps.insert(backing.as_raw() as usize);
                    committed_bytes += host.device.GetResourceAllocationInfo(0, &[d]).SizeInBytes;
                }
                textures.push(t);
            }
        }
        drop(pool);
        let placed_bytes = heaps.len() as u64 * PAGE;
        assert!(placed_bytes < committed_bytes / 2);
        eprintln!(
            "small textures: {} resources, {} distinct 64 KiB heaps, {placed_bytes} placed bytes vs {committed_bytes} committed bytes",
            textures.len(),
            heaps.len()
        );

        let list = host.new_list().unwrap();
        let mut expected = Vec::new();
        for (index, texture) in textures.iter().enumerate() {
            let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let (mut rows, mut row_bytes, mut total) = (0, 0, 0);
            let value = index as u8 + 1;
            unsafe {
                host.device.GetCopyableFootprints(
                    &texture.GetDesc(),
                    0,
                    1,
                    0,
                    Some(&mut footprint),
                    Some(&mut rows),
                    Some(&mut row_bytes),
                    Some(&mut total),
                );
                let (upload, mapped) = host.upload_buffer(total).unwrap();
                mapped.write_bytes(value, total as usize);
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&upload),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: footprint,
                    },
                };
                let to = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(texture),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        SubresourceIndex: 0,
                    },
                };
                list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
                list.keep(&upload);
                list.list.ResourceBarrier(&[crate::host::transition(
                    texture,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
            }
            expected.push((rows, row_bytes, value));
        }
        // Read after all writes so later placements cannot silently alias earlier ones.
        let reads: Vec<_> = textures
            .iter()
            .map(|texture| crate::capture::read_back(&host.device, &list.list, texture).unwrap())
            .collect();
        list.close();
        host.execute(&[&list]);
        for ((buffer, footprint), (rows, row_bytes, value)) in reads.into_iter().zip(expected) {
            unsafe {
                let mut data = std::ptr::null_mut();
                buffer.Map(0, None, Some(&mut data)).unwrap();
                for row in 0..rows {
                    let ptr = data
                        .cast::<u8>()
                        .add((row * footprint.Footprint.RowPitch) as usize);
                    assert!(
                        std::slice::from_raw_parts(ptr, row_bytes as usize)
                            .iter()
                            .all(|&b| b == value)
                    );
                }
                buffer.Unmap(0, None);
            }
        }
    }
}
