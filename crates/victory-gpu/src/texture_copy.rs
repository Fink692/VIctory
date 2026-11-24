//! GPU-ordered copies between title textures and linear buffer footprints.
//!
//! A CPU upload snapshot cannot observe a shader/query/copy that writes its
//! source earlier in the same submission. Both directions therefore use
//! shadow buffers and GPU staging. Staging also translates tight title row
//! pitches and unaligned title offsets to D3D12's 256/512-byte requirements.
//! Destination rows are initialized before a partial copy, preserving their
//! untouched pixels; only payload bytes return to title rows when pitches differ.

use std::sync::Arc;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

use crate::host::{HostList, host};
use crate::list::CopyLocation;
use crate::object::This;
use crate::resource::PlacedFootprint;
use crate::shadow::Shadow;

const SUBRESOURCE: u32 = 0;
const FOOTPRINT: u32 = 1;

struct Buffer {
    shadow: Arc<Shadow>,
    address: u64,
    footprint: PlacedFootprint,
    bytes: u64,
    shape: [u32; 3],
}

struct Texture {
    title: This,
    resource: ID3D12Resource,
    subresource: u32,
    shape: [u32; 3],
    format: u32,
    footprint: Option<PlacedFootprint>,
    samples: u32,
    depth_stencil: bool,
}

enum End {
    Buffer(Buffer),
    Texture(Texture),
}

impl End {
    fn new(location: &CopyLocation) -> Result<Self, &'static str> {
        match location.kind {
            FOOTPRINT => {
                let footprint = location.footprint;
                // These console-only layouts need a pixel conversion, not a
                // byte copy into an incompatible host format.
                if matches!(footprint.format, 116..=120 | 189..=191) {
                    return Err("console-only footprint format needs conversion");
                }
                // Depth/stencil parent formats describe two planes, not a
                // single placed image. Native planes use R32 or R8 instead.
                if matches!(crate::resource::family(footprint.format), 19 | 44) {
                    return Err("a depth/stencil footprint must select a plane format");
                }
                let bytes = footprint.bytes().ok_or("invalid buffer footprint")?;
                let shape = footprint_shape(footprint).ok_or("footprint dimensions overflow")?;
                let (data, len) = crate::resource::buffer_bytes(location.resource)
                    .ok_or("a footprint must refer to a buffer")?;
                if footprint
                    .offset
                    .checked_add(bytes)
                    .is_none_or(|end| end > len)
                {
                    return Err("texture copy footprint exceeds its title buffer");
                }
                let address = (data as u64)
                    .checked_add(footprint.offset)
                    .ok_or("buffer address overflow")?;
                let shadow = crate::shadow::find(address, bytes).ok_or("no buffer shadow")?;
                Ok(Self::Buffer(Buffer {
                    shadow,
                    address,
                    footprint,
                    bytes,
                    shape,
                }))
            }
            SUBRESOURCE => {
                let resource = crate::resource::host_resource(location.resource)
                    .ok_or("a subresource must refer to a texture")?
                    .clone();
                // SAFETY: a live host texture and its matching feature structure.
                let desc = unsafe { resource.GetDesc() };
                let mut info = D3D12_FEATURE_DATA_FORMAT_INFO {
                    Format: desc.Format,
                    ..Default::default()
                };
                let host = host().ok_or("no host device")?;
                // SAFETY: a live device and the matching, writable feature structure.
                unsafe {
                    host.device.CheckFeatureSupport(
                        D3D12_FEATURE_FORMAT_INFO,
                        (&raw mut info).cast(),
                        size_of_val(&info) as u32,
                    )
                }
                .map_err(|_| "cannot determine texture planes")?;
                let mips = u32::from(desc.MipLevels);
                let slices = if desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D {
                    1
                } else {
                    u32::from(desc.DepthOrArraySize)
                };
                let count = mips
                    .checked_mul(slices)
                    .and_then(|n| n.checked_mul(u32::from(info.PlaneCount)))
                    .ok_or("texture subresource count overflow")?;
                let subresource = location.footprint.offset as u32;
                if mips == 0 || subresource >= count {
                    return Err("texture subresource out of bounds");
                }
                let mip = subresource % mips;
                let width = u32::try_from((desc.Width >> mip).max(1))
                    .map_err(|_| "texture width out of range")?;
                let height = (desc.Height >> mip).max(1);
                let depth = if desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D {
                    (u32::from(desc.DepthOrArraySize) >> mip).max(1)
                } else {
                    1
                };
                // The selected plane may have a different format and size
                // from the parent texture. For example, D24S8 has an R32
                // depth footprint and an R8 stencil footprint. BC footprints
                // include the complete blocks of small mip levels.
                let footprint = if desc.SampleDesc.Count == 1 {
                    let mut layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
                    let mut bytes = u64::MAX;
                    // SAFETY: one checked subresource and one output layout.
                    unsafe {
                        host.device.GetCopyableFootprints(
                            &desc,
                            subresource,
                            1,
                            0,
                            Some(&raw mut layout),
                            None,
                            None,
                            Some(&raw mut bytes),
                        )
                    };
                    if bytes == u64::MAX || bytes == 0 {
                        return Err("no native footprint for the texture plane");
                    }
                    Some(PlacedFootprint {
                        offset: layout.Offset,
                        format: layout.Footprint.Format.0 as u32,
                        width: layout.Footprint.Width,
                        height: layout.Footprint.Height,
                        depth: layout.Footprint.Depth,
                        row_pitch: layout.Footprint.RowPitch,
                    })
                } else {
                    None
                };
                let shape = match footprint {
                    Some(f) if crate::resource::element(f.format).is_none_or(|(_, b)| b == 1) => {
                        [f.width, f.height, f.depth]
                    }
                    _ => [width, height, depth],
                };
                Ok(Self::Texture(Texture {
                    title: location.resource,
                    resource,
                    subresource,
                    shape,
                    format: desc.Format.0 as u32,
                    footprint,
                    samples: desc.SampleDesc.Count,
                    depth_stencil: desc.Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL.0 != 0,
                }))
            }
            _ => Err("unknown texture copy location type"),
        }
    }

    fn shape(&self) -> [u32; 3] {
        match self {
            Self::Buffer(b) => b.shape,
            Self::Texture(t) => t
                .footprint
                .map_or(t.shape, |f| [f.width, f.height, f.depth]),
        }
    }

    /// Logical dimensions are also legal box edges for a partial BC block.
    fn logical_shape(&self) -> [u32; 3] {
        match self {
            Self::Buffer(b) => [b.footprint.width, b.footprint.height, b.footprint.depth],
            Self::Texture(t) => t.shape,
        }
    }

    fn format(&self) -> u32 {
        match self {
            Self::Buffer(b) => b.footprint.format,
            Self::Texture(t) => t.footprint.map_or(t.format, |f| f.format),
        }
    }

    fn samples(&self) -> u32 {
        match self {
            Self::Buffer(_) => 1,
            Self::Texture(t) => t.samples,
        }
    }

    fn whole_only(&self) -> bool {
        matches!(self, Self::Texture(t) if t.depth_stencil || t.samples > 1)
    }
}

fn footprint_shape(f: PlacedFootprint) -> Option<[u32; 3]> {
    let (_, block) = crate::resource::element(f.format)?;
    let block = block as u32;
    Some([
        f.width.checked_next_multiple_of(block)?,
        f.height.checked_next_multiple_of(block)?,
        f.depth,
    ])
}

fn footprint_location(
    resource: &ID3D12Resource,
    f: PlacedFootprint,
) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: borrowed COM pointer; the owning endpoint/state table outlives recording.
        pResource: unsafe { std::mem::transmute_copy(resource) },
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: f.offset,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: DXGI_FORMAT(f.format as i32),
                    Width: f.width,
                    Height: f.height,
                    Depth: f.depth,
                    RowPitch: f.row_pitch,
                },
            },
        },
    }
}

fn texture_location(texture: &Texture) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: a borrowed pointer; HostList::transition keeps the host copy alive.
        pResource: unsafe { std::mem::transmute_copy(&texture.resource) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: texture.subresource,
        },
    }
}

struct Stage {
    resource: ID3D12Resource,
    footprint: PlacedFootprint,
}

impl Stage {
    fn new(footprint: PlacedFootprint) -> Result<Self, &'static str> {
        let [width, height, depth] =
            footprint_shape(footprint).ok_or("staging dimensions overflow")?;
        let (element, block) =
            crate::resource::element(footprint.format).ok_or("unsupported footprint format")?;
        let row_bytes = u64::from(footprint.width)
            .div_ceil(block)
            .checked_mul(element)
            .ok_or("row size overflow")?;
        let row_pitch = u32::try_from(
            row_bytes
                .checked_next_multiple_of(256)
                .ok_or("row pitch overflow")?,
        )
        .map_err(|_| "row pitch out of range")?;
        let footprint = PlacedFootprint {
            offset: 0,
            width,
            height,
            depth,
            row_pitch,
            ..footprint
        };
        let bytes = footprint.bytes().ok_or("staging footprint overflow")?;
        let (resource, _) = host()
            .and_then(|h| h.shared_buffer(bytes))
            .ok_or("cannot allocate texture-copy staging")?;
        Ok(Self {
            resource,
            footprint,
        })
    }

    fn location(&self) -> D3D12_TEXTURE_COPY_LOCATION {
        footprint_location(&self.resource, self.footprint)
    }

    fn rows(&self, list: &HostList, buffer: &Buffer, into_stage: bool) {
        let (source, target, from, to, source_pitch, target_pitch) = if into_stage {
            (
                &buffer.shadow.buffer,
                &self.resource,
                buffer.shadow.offset(buffer.address),
                0,
                buffer.footprint.row_pitch,
                self.footprint.row_pitch,
            )
        } else {
            (
                &self.resource,
                &buffer.shadow.buffer,
                0,
                buffer.shadow.offset(buffer.address),
                self.footprint.row_pitch,
                buffer.footprint.row_pitch,
            )
        };
        list.buffer_state(source, D3D12_RESOURCE_STATE_COPY_SOURCE);
        list.buffer_state(target, D3D12_RESOURCE_STATE_COPY_DEST);
        // SAFETY: the original title range and both footprint spans were checked.
        // The state table holds both buffers until the list is reset.
        unsafe {
            if source_pitch == target_pitch {
                list.list
                    .CopyBufferRegion(target, to, source, from, buffer.bytes);
            } else {
                let (element, block) = crate::resource::element(buffer.footprint.format).unwrap();
                let bytes = u64::from(buffer.footprint.width).div_ceil(block) * element;
                let rows = u64::from(buffer.footprint.height).div_ceil(block)
                    * u64::from(buffer.footprint.depth);
                for row in 0..rows {
                    list.list.CopyBufferRegion(
                        target,
                        to + row * u64::from(target_pitch),
                        source,
                        from + row * u64::from(source_pitch),
                        bytes,
                    );
                }
            }
        }
    }
}

/// All source reads, optional row repacking, texture copies and readback
/// writes happen in native command order, including across submitted lists.
pub(crate) fn record(
    list: &HostList,
    dst: &CopyLocation,
    at: [u32; 3],
    src: &CopyLocation,
    region: Option<[u32; 6]>,
) -> Result<(), &'static str> {
    let (source, target) = (End::new(src)?, End::new(dst)?);
    let shape = source.shape();
    let bounds = region.unwrap_or([0, 0, 0, shape[0], shape[1], shape[2]]);
    let target_shape = target.shape();
    let mut empty = false;
    for axis in 0..3 {
        let (first, last) = (bounds[axis], bounds[axis + 3]);
        if first > last
            || last > shape[axis]
            || at[axis]
                .checked_add(last - first)
                .is_none_or(|end| end > target_shape[axis])
        {
            return Err("texture copy box or destination is out of bounds");
        }
        empty |= first == last;
    }
    if empty {
        return Ok(());
    }
    if crate::resource::family(source.format()) != crate::resource::family(target.format()) {
        return Err("texture copy formats are not in the same family");
    }
    if let (End::Texture(s), End::Texture(d)) = (&source, &target)
        && crate::resource::family(s.format) != crate::resource::family(d.format)
    {
        return Err("texture parent formats are not in the same family");
    }
    if source.samples() != target.samples() {
        return Err("texture copy sample counts differ");
    }
    if (source.whole_only() || target.whole_only())
        && (region.is_some() || at != [0; 3] || source.shape() != target.shape())
    {
        return Err("depth/stencil and multisample copies require whole subresources");
    }
    if let Some((_, block)) = crate::resource::element(source.format())
        && block > 1
    {
        for axis in 0..2 {
            if !u64::from(bounds[axis]).is_multiple_of(block)
                || !u64::from(at[axis]).is_multiple_of(block)
                || (bounds[axis + 3] != source.logical_shape()[axis]
                    && !u64::from(bounds[axis + 3]).is_multiple_of(block))
            {
                return Err("compressed texture copy is not block aligned");
            }
        }
    }
    let source_stage = match &source {
        End::Buffer(b) => Some(Stage::new(b.footprint)?),
        End::Texture(t) if matches!(&target, End::Texture(d) if d.resource == t.resource) => {
            if t.samples != 1 || t.depth_stencil {
                return Err(
                    "same-resource depth/multisample copy needs subresource state tracking",
                );
            }
            if matches!(&target, End::Texture(d) if d.subresource == t.subresource) {
                return Err("source and destination are the same texture subresource");
            }
            Some(Stage::new(
                t.footprint.ok_or("no staging texture footprint")?,
            )?)
        }
        _ => None,
    };
    let target_stage = match &target {
        End::Buffer(b) => Some(Stage::new(b.footprint)?),
        End::Texture(_) => None,
    };

    let from = match &source {
        End::Buffer(buffer) => {
            buffer.shadow.refresh(list, buffer.address, buffer.bytes);
            let stage = source_stage.as_ref().unwrap();
            stage.rows(list, buffer, true);
            list.buffer_state(&stage.resource, D3D12_RESOURCE_STATE_COPY_SOURCE);
            stage.location()
        }
        End::Texture(texture) => {
            crate::list::ensure_state(list, texture.title, 0x800);
            let location = texture_location(texture);
            if let Some(stage) = &source_stage {
                list.buffer_state(&stage.resource, D3D12_RESOURCE_STATE_COPY_DEST);
                // SAFETY: both entire subresources fit their checked footprints.
                unsafe {
                    list.list
                        .CopyTextureRegion(&stage.location(), 0, 0, 0, &location, None)
                };
                list.buffer_state(&stage.resource, D3D12_RESOURCE_STATE_COPY_SOURCE);
                stage.location()
            } else {
                location
            }
        }
    };
    let to = match &target {
        End::Buffer(buffer) => {
            buffer.shadow.refresh(list, buffer.address, buffer.bytes);
            let stage = target_stage.as_ref().unwrap();
            // Preserve pixels outside the copy box before the GPU overwrites it.
            stage.rows(list, buffer, true);
            stage.location()
        }
        End::Texture(texture) => {
            crate::list::ensure_state(list, texture.title, 0x400);
            crate::texture::host_written(texture.title);
            texture_location(texture)
        }
    };
    let native_box = region.map(|mut b| {
        // A staged BC image has complete-block dimensions. Its source box
        // must name the complete last block, even when the title box ended
        // at the original texture's smaller logical mip edge. Bounds and
        // block-aligned starts were checked before recording any commands.
        if source_stage.is_some()
            && let Some((_, block)) = crate::resource::element(source.format())
            && block > 1
        {
            b[3] = b[3].next_multiple_of(block as u32);
            b[4] = b[4].next_multiple_of(block as u32);
        }
        D3D12_BOX {
            left: b[0],
            top: b[1],
            front: b[2],
            right: b[3],
            bottom: b[4],
            back: b[5],
        }
    });
    #[cfg(feature = "diagnostics")]
    list.note_op(|| {
        format!(
            "GPU texture copy {:?}/{} to {:?}/{} at {at:?} box {region:?}",
            src.resource, src.kind, dst.resource, dst.kind
        )
    });
    // SAFETY: both locations, copy extents, formats and states were checked;
    // all native buffers/textures are retained by this list's state tables.
    unsafe {
        list.list.CopyTextureRegion(
            &to,
            at[0],
            at[1],
            at[2],
            &from,
            native_box.as_ref().map(std::ptr::from_ref),
        );
    }
    if let End::Buffer(buffer) = &target {
        target_stage.as_ref().unwrap().rows(list, buffer, false);
        buffer.shadow.gpu_writes(list, buffer.address, buffer.bytes);
    }
    Ok(())
}
