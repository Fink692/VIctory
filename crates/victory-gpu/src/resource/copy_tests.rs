//! Authored GPU-copy fixtures; no title inputs or expected game pixels.

use super::*;
use crate::list::CopyLocation;

struct Owned(This);

impl Drop for Owned {
    fn drop(&mut self) {
        resource_release(self.0);
    }
}

fn make(width: u64, height: u32, format: u32, mips: u16) -> Owned {
    let desc = ResourceDesc {
        dimension: if format == 0 { BUFFER } else { 3 },
        alignment: 0,
        width,
        height,
        depth_or_array_size: 1,
        mip_levels: mips,
        format,
        sample_count: 1,
        sample_quality: 0,
        layout: if format == 0 { 1 } else { 0 },
        flags: match format {
            20 | 40 | 45 | 55 => 2,
            28 => 1,
            _ => 0,
        },
    };
    let properties = HeapProperties {
        kind: 1,
        cpu_pages: 0,
        pool: 0,
        creation_nodes: 0,
        visible_nodes: 0,
    };
    let mut result = std::ptr::null();
    assert_eq!(
        create_committed(
            std::ptr::null(),
            &properties,
            0,
            &desc,
            0,
            std::ptr::null(),
            &RESOURCE_IIDS[0],
            &mut result
        ),
        S_OK
    );
    Owned(result)
}

fn bytes(resource: &Owned, value: u8) -> u64 {
    let (data, len) = buffer_bytes(resource.0).unwrap();
    // SAFETY: the resource owns this live allocation and execution has not started.
    unsafe { std::slice::from_raw_parts_mut(data, len as usize).fill(value) };
    data as u64
}

fn read(resource: &Owned) -> Vec<u8> {
    let (data, len) = buffer_bytes(resource.0).unwrap();
    // SAFETY: callers only read after the host's completion wait.
    unsafe { std::slice::from_raw_parts(data, len as usize).to_vec() }
}

fn placed(resource: &Owned, offset: u64, width: u32, height: u32, pitch: u32) -> CopyLocation {
    CopyLocation {
        resource: resource.0,
        kind: 1,
        footprint: PlacedFootprint {
            offset,
            format: 28,
            width,
            height,
            depth: 1,
            row_pitch: pitch,
        },
    }
}

fn placed_format(
    resource: &Owned,
    offset: u64,
    format: u32,
    width: u32,
    height: u32,
    pitch: u32,
) -> CopyLocation {
    let mut location = placed(resource, offset, width, height, pitch);
    location.footprint.format = format;
    location
}

fn image(resource: &Owned, subresource: u32) -> CopyLocation {
    CopyLocation {
        resource: resource.0,
        kind: 0,
        footprint: PlacedFootprint {
            offset: u64::from(subresource),
            format: 0,
            width: 0,
            height: 0,
            depth: 0,
            row_pitch: 0,
        },
    }
}

/// An earlier GPU copy writes the upload source. A later texture readback
/// and buffer copy consume those pixels before CPU copy-back runs. Tight
/// pitches, unaligned offsets, partial boxes and destination guards all matter.
#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn gpu_texture_copies_preserve_order_and_guards() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let input = make(4096, 1, 0, 1);
    let working = make(4096, 1, 0, 1);
    let output = make(4096, 1, 0, 1);
    let after = make(4096, 1, 0, 1);
    let texture = make(4, 4, 28, 1);
    let a = bytes(&input, 0xee);
    let b = bytes(&working, 0x3c);
    let c = bytes(&output, 0xa7);
    let d = bytes(&after, 0x6d);
    let pixel = |x: usize, y: usize| [x as u8 + 1, y as u8 + 11, (x + y * 4) as u8 + 31, 255];
    for y in 0..4 {
        for x in 0..4 {
            // SAFETY: the four-byte pixel fits the authored source allocation.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pixel(x, y).as_ptr(),
                    (a + 7 + (y * 20 + x * 4) as u64) as *mut u8,
                    4,
                )
            };
        }
    }
    let list = host.new_list().unwrap();
    assert!(crate::list::copy_buffers(&list, b + 3, a + 7, 76));
    crate::texture_copy::record(
        &list,
        &image(&texture, 0),
        [0; 3],
        &placed(&working, 3, 4, 4, 20),
        None,
    )
    .unwrap();
    crate::texture_copy::record(
        &list,
        &placed(&output, 5, 6, 6, 28),
        [2, 3, 0],
        &image(&texture, 0),
        Some([1, 1, 0, 3, 3, 1]),
    )
    .unwrap();
    assert!(crate::list::copy_buffers(&list, d + 11, c + 5, 164));
    list.close();
    assert!(read(&working).iter().all(|&v| v == 0x3c));
    assert!(read(&output).iter().all(|&v| v == 0xa7));
    host.execute(&[&list]);
    let mut expected = vec![0xa7; 4096];
    for y in 0..2 {
        for x in 0..2 {
            let at = 5 + (y + 3) * 28 + (x + 2) * 4;
            expected[at..at + 4].copy_from_slice(&pixel(x + 1, y + 1));
        }
    }
    assert_eq!(
        read(&output),
        expected,
        "partial readback changed pixels/padding outside its box"
    );
    let mut expected_after = vec![0x6d; 4096];
    expected_after[11..175].copy_from_slice(&expected[5..169]);
    assert_eq!(
        read(&after),
        expected_after,
        "later GPU consumer read stale title memory"
    );
}

/// Recording the consumer first must not change submission ordering. Both
/// directions use ordinary 256-byte footprints here; only the final row's
/// real bytes belong to the source range, and guards must survive readback.
#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn texture_readback_crosses_lists_and_reuses_subresources() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let source = make(4096, 1, 0, 1);
    let output = make(4096, 1, 0, 1);
    let texture = make(4, 4, 28, 2);
    let a = bytes(&source, 0x22);
    bytes(&output, 0x91);
    // Distinct pixel values are copied into mip1 through an intermediate
    // GPU footprint, so a whole-resource source/destination state collision
    // on this texture cannot silently pass.
    for y in 0..4 {
        for x in 0..4 {
            let value = [(x + y * 4) as u8, 77, 143, 255];
            // SAFETY: the four-byte authored pixel fits this live input buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    value.as_ptr(),
                    (a + 512 + (y * 256 + x * 4) as u64) as *mut u8,
                    4,
                )
            };
        }
    }
    let consumer = host.new_list().unwrap();
    crate::texture_copy::record(
        &consumer,
        &placed(&output, 512, 2, 2, 256),
        [0; 3],
        &image(&texture, 1),
        None,
    )
    .unwrap();
    consumer.close();
    let producer = host.new_list().unwrap();
    crate::texture_copy::record(
        &producer,
        &image(&texture, 0),
        [0; 3],
        &placed(&source, 512, 4, 4, 256),
        None,
    )
    .unwrap();
    crate::texture_copy::record(
        &producer,
        &image(&texture, 1),
        [0; 3],
        &image(&texture, 0),
        Some([1, 1, 0, 3, 3, 1]),
    )
    .unwrap();
    producer.close();
    host.execute(&[&producer, &consumer]);
    let mut expected = vec![0x91; 4096];
    for y in 0..2 {
        for x in 0..2 {
            let at = 512 + y * 256 + x * 4;
            expected[at..at + 4].copy_from_slice(&[((x + 1) + (y + 1) * 4) as u8, 77, 143, 255]);
        }
    }
    assert_eq!(read(&output), expected);
}

#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn invalid_texture_copy_ranges_leave_destination_unchanged() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let buffer = make(256, 1, 0, 1);
    let texture = make(4, 4, 28, 1);
    bytes(&buffer, 0x5e);
    let list = host.new_list().unwrap();
    assert!(
        crate::texture_copy::record(
            &list,
            &placed(&buffer, 240, 4, 4, 16),
            [0; 3],
            &image(&texture, 0),
            None
        )
        .is_err()
    );
    assert!(
        crate::texture_copy::record(
            &list,
            &placed(&buffer, 0, 4, 4, 16),
            [3, 0, 0],
            &image(&texture, 0),
            Some([0, 0, 0, 2, 2, 1])
        )
        .is_err()
    );
    assert!(
        crate::texture_copy::record(
            &list,
            &placed(&buffer, 0, 4, 4, 16),
            [0; 3],
            &image(&texture, 99),
            None
        )
        .is_err()
    );
    assert!(
        crate::texture_copy::record(
            &list,
            &placed(&buffer, 0, 4, 4, 16),
            [0; 3],
            &image(&texture, 0),
            Some([3, 0, 0, 2, 2, 1])
        )
        .is_err()
    );
    list.close();
    host.execute(&[&list]);
    assert!(read(&buffer).iter().all(|&v| v == 0x5e));
}

/// BC1 mip footprints remain one complete 4x4 block when their logical
/// extent is 2x2 or 1x1. Both the full padded footprint and a logical-edge
/// source box must copy that block without changing buffer guards.
#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn block_compressed_small_mips_round_trip() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let input = make(512, 1, 0, 1);
    let output = make(512, 1, 0, 1);
    let texture = make(4, 4, 71, 3);
    let data = bytes(&input, 0x5a);
    bytes(&output, 0xa3);
    let block = |mip: usize| [mip as u8 + 1, 17, 39, 53, 71, 91, 113, 137];
    let list = host.new_list().unwrap();
    for mip in 0..3 {
        let offset = 7 + mip * 64;
        // SAFETY: this eight-byte authored BC1 block fits the input buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(block(mip).as_ptr(), (data + offset as u64) as *mut u8, 8)
        };
        let (size, region) = if mip == 2 {
            (1, Some([0, 0, 0, 1, 1, 1]))
        } else {
            (4, None)
        };
        crate::texture_copy::record(
            &list,
            &image(&texture, mip as u32),
            [0; 3],
            &placed_format(&input, offset as u64, 71, size, size, 12),
            region,
        )
        .unwrap();
    }
    // Observe the logical-sized buffer upload before overwriting mip 2.
    crate::texture_copy::record(
        &list,
        &placed_format(&output, 267, 71, 4, 4, 12),
        [0; 3],
        &image(&texture, 2),
        None,
    )
    .unwrap();
    // This exercises the same-resource staging footprint for a small mip.
    crate::texture_copy::record(
        &list,
        &image(&texture, 2),
        [0; 3],
        &image(&texture, 1),
        Some([0, 0, 0, 2, 2, 1]),
    )
    .unwrap();
    let mut expected = vec![0xa3; 512];
    expected[267..275].copy_from_slice(&block(2));
    for mip in 0..3 {
        let offset = 11 + mip * 64;
        crate::texture_copy::record(
            &list,
            &placed_format(&output, offset as u64, 71, 4, 4, 12),
            [0; 3],
            &image(&texture, mip as u32),
            None,
        )
        .unwrap();
        expected[offset..offset + 8].copy_from_slice(&block(if mip == 2 { 1 } else { mip }));
    }
    crate::texture_copy::record(
        &list,
        &placed_format(&output, 203, 71, 4, 4, 12),
        [0; 3],
        &image(&texture, 2),
        Some([0, 0, 0, 1, 1, 1]),
    )
    .unwrap();
    expected[203..211].copy_from_slice(&block(1));
    assert!(
        crate::texture_copy::record(
            &list,
            &placed_format(&output, 331, 71, 4, 4, 12),
            [0; 3],
            &image(&texture, 2),
            Some([0, 0, 0, 2, 1, 1]),
        )
        .is_err(),
        "an incomplete block edge that is not the logical mip edge is invalid"
    );
    list.close();
    host.execute(&[&list]);
    assert_eq!(read(&output), expected);
}

/// The two D24S8 subresources have different native footprint formats:
/// R32 for the depth plane and R8 for stencil. A packed D24S8 footprint
/// cannot be passed through to native CopyTextureRegion.
#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn depth_stencil_copies_use_selected_plane_formats() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let input = make(512, 1, 0, 1);
    let output = make(512, 1, 0, 1);
    let texture = make(2, 2, 45, 1);
    let data = bytes(&input, 0x6c);
    bytes(&output, 0xb7);
    // D24X8 requires its unused high byte to be zero on upload.
    let depth = [
        0x00_00_00_00u32,
        0x00_01_23_45,
        0x00_80_00_00,
        0x00_ff_ff_ff,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();
    let stencil = [0x12u8, 0x34, 0x80, 0xfe];
    // SAFETY: each authored plane fits its separate input buffer range.
    unsafe {
        std::ptr::copy_nonoverlapping(depth.as_ptr(), (data + 3) as *mut u8, depth.len());
        std::ptr::copy_nonoverlapping(stencil.as_ptr(), (data + 67) as *mut u8, stencil.len());
    }
    let list = host.new_list().unwrap();
    for (plane, format, offset, pitch) in [(0, 42, 3, 8), (1, 62, 67, 2)] {
        crate::texture_copy::record(
            &list,
            &image(&texture, plane),
            [0; 3],
            &placed_format(&input, offset, format, 2, 2, pitch),
            None,
        )
        .unwrap();
        crate::texture_copy::record(
            &list,
            &placed_format(&output, offset, format, 2, 2, pitch),
            [0; 3],
            &image(&texture, plane),
            None,
        )
        .unwrap();
    }
    assert!(
        crate::texture_copy::record(
            &list,
            &placed_format(&output, 131, 45, 2, 2, 8),
            [0; 3],
            &image(&texture, 0),
            None,
        )
        .is_err()
    );
    assert!(
        crate::texture_copy::record(
            &list,
            &placed_format(&output, 131, 62, 2, 2, 2),
            [0; 3],
            &image(&texture, 0),
            None,
        )
        .is_err(),
        "the stencil footprint format does not describe the depth plane"
    );
    list.close();
    host.execute(&[&list]);
    let mut expected = vec![0xb7; 512];
    expected[3..19].copy_from_slice(&depth);
    expected[67..71].copy_from_slice(&stencil);
    assert_eq!(read(&output), expected);
}
