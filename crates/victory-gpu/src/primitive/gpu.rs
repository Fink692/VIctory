//! GPU-ordered expansion of the existing input-based rectangle shortcut.
//! Exact input bytes are staged before shader reads, including a zero tail
//! for unaligned DWORD loads. Each output DWORD has one writer even when
//! odd strides put neighboring vertices in that same word.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_IEEE_STRICTNESS, D3DCompile};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};

#[cfg(test)]
mod tests;

pub(crate) struct Expanded {
    /// Kept by the list's tracked states as well as this return value.
    pub(crate) buffer: ID3D12Resource,
    pub(crate) view: D3D12_VERTEX_BUFFER_VIEW,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct Layout {
    stride: u32,
    elements: Vec<(u32, u32)>,
}

struct Pipeline {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

const THREADS: u64 = 64;
const MAX_GROUPS: u64 = 65_535;

struct Plan {
    from: u64,
    input: u64,
    output: u32,
    padded_input: u64,
    padded_output: u64,
    groups: [u32; 2],
    row_words: u32,
}

impl Plan {
    fn new(
        address: u64,
        count: u32,
        first: u32,
        vertex_size: u32,
        stride: u32,
    ) -> Result<Option<Self>, &'static str> {
        let rectangles = u64::from(count / 3);
        if rectangles == 0 {
            return Ok(None);
        }
        if address == 0 || stride == 0 {
            return Err("rectangle vertices have no address or stride");
        }
        let stride = u64::from(stride);
        let end = (u64::from(first) + u64::from(count))
            .checked_mul(stride)
            .ok_or("rectangle vertex range overflow")?;
        if end > u64::from(vertex_size) || address.checked_add(u64::from(vertex_size)).is_none() {
            return Err("rectangle vertices exceed their declared buffer");
        }
        let from = address
            .checked_add(u64::from(first) * stride)
            .ok_or("rectangle source address overflow")?;
        let input = rectangles * 3 * stride;
        let output = rectangles
            .checked_mul(6)
            .and_then(|n| n.checked_mul(stride))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("expanded rectangle vertices exceed the native view limit")?;
        let padded_input = input.next_multiple_of(4);
        let padded_output = u64::from(output).next_multiple_of(4);
        let words = padded_output / 4;
        let groups = words.div_ceil(THREADS);
        let x = groups.min(MAX_GROUPS);
        let y = groups.div_ceil(x);
        if y > MAX_GROUPS || x * THREADS * y > u64::from(u32::MAX) {
            return Err("rectangle expansion exceeds native dispatch limits");
        }
        Ok(Some(Self {
            from,
            input,
            output,
            padded_input,
            padded_output,
            groups: [x as u32, y as u32],
            row_words: (x * THREADS) as u32,
        }))
    }
}

/// Expands the complete rectangles in a declared vertex view. A zero or
/// incomplete rectangle is a no-op. Rejects invalid ranges before creating
/// host resources or recording work. The returned buffer is ready for vertex
/// reads, and all resources remain alive until `list` is reset.
///
/// Replaces the native compute root signature and pipeline, but never changes
/// descriptor heaps. The caller must restore its pipeline/cache bookkeeping
/// before the next title draw or dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand(
    list: &HostList,
    address: u64,
    count: u32,
    first: u32,
    vertex_size: u32,
    stride: u32,
    elements: &[(u32, u32)],
) -> Result<Option<Expanded>, &'static str> {
    let Some(plan) = Plan::new(address, count, first, vertex_size, stride)? else {
        return Ok(None);
    };
    // Match ordinary vertex binding's full-view lookup. An overlapping
    // smaller title resource may cover the address without covering this
    // view, and must not select different backing for expansion.
    let source = crate::shadow::find(address, u64::from(vertex_size))
        .ok_or("rectangle source has no host copy")?;
    if source.left(plan.from) < plan.input {
        return Err("rectangle vertices exceed their host buffer");
    }
    let pipeline = pipeline(stride, elements).ok_or("rectangle expansion pipeline unavailable")?;
    let host = host().ok_or("rectangle expansion has no host device")?;
    let (input, input_data) = host
        .shared_buffer(plan.padded_input)
        .ok_or("rectangle staging allocation failed")?;
    let (buffer, _) = host
        .shared_buffer(plan.padded_output)
        .ok_or("rectangle output allocation failed")?;
    // GPU copy fills the exact source range. Its tail remains zero for the
    // final DWORD load, even if the original shadow has an exact byte width.
    // SAFETY: a new, mapped resource not yet submitted to the GPU.
    unsafe { std::ptr::write_bytes(input_data, 0, plan.padded_input as usize) };
    source.refresh(list, plan.from, plan.input);
    list.buffer_state(&source.buffer, D3D12_RESOURCE_STATE_COPY_SOURCE);
    list.buffer_state(&input, D3D12_RESOURCE_STATE_COPY_DEST);
    // SAFETY: ranges checked above and both native buffers retained by the
    // list's state map; only real source bytes are copied.
    unsafe {
        list.list.CopyBufferRegion(
            &input,
            0,
            &source.buffer,
            source.offset(plan.from),
            plan.input,
        )
    };
    list.buffer_state(&input, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
    list.buffer_state(&buffer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    list.keep(&pipeline.root);
    list.keep(&pipeline.pso);
    #[cfg(feature = "diagnostics")]
    list.note_op(|| {
        format!(
            "expand {} rectangles, stride {stride}, {} bytes",
            count / 3,
            plan.output
        )
    });
    let constants = [plan.output, plan.row_words];
    // SAFETY: the authored root signature matches these parameters; the
    // pipeline and buffers are retained and dispatch bounds checked above.
    let gpu = unsafe {
        list.list.SetComputeRootSignature(&pipeline.root);
        list.list.SetPipelineState(&pipeline.pso);
        list.list
            .SetComputeRoot32BitConstants(0, 2, constants.as_ptr().cast(), 0);
        list.list
            .SetComputeRootShaderResourceView(1, input.GetGPUVirtualAddress());
        let gpu = buffer.GetGPUVirtualAddress();
        list.list.SetComputeRootUnorderedAccessView(2, gpu);
        list.list.Dispatch(plan.groups[0], plan.groups[1], 1);
        gpu
    };
    list.buffer_state(&buffer, D3D12_RESOURCE_STATE_GENERIC_READ);
    Ok(Some(Expanded {
        buffer,
        view: D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: gpu,
            SizeInBytes: plan.output,
            StrideInBytes: stride,
        },
    }))
}

fn pipeline(stride: u32, elements: &[(u32, u32)]) -> Option<Arc<Pipeline>> {
    static PIPELINES: OnceLock<Mutex<HashMap<Layout, Arc<Pipeline>>>> = OnceLock::new();
    let key = Layout {
        stride,
        elements: elements.to_vec(),
    };
    let mut pipelines = PIPELINES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(pipeline) = pipelines.get(&key) {
        return Some(pipeline.clone());
    }
    let source = shader(&key);
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: generated source contains only numeric layout constants;
    // source, static names and output pointers live through compilation.
    let compiled = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("victory-rectangle-expansion"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            D3DCOMPILE_IEEE_STRICTNESS,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = compiled {
        let message = errors.map(|blob| {
            // SAFETY: the compiler blob owns the returned byte range.
            String::from_utf8_lossy(unsafe {
                std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
            })
            .into_owned()
        });
        tracing::warn!(%error, ?message, "rectangle expansion shader failed");
        return None;
    }
    let code = code?;
    let device = &host()?.device;
    // SAFETY: live compiled bytes contain the authored root signature.
    let made = unsafe {
        let bytes =
            std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize());
        device
            .CreateRootSignature::<ID3D12RootSignature>(0, bytes)
            .and_then(|root| {
                device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&root),
                        CS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: bytes.as_ptr().cast(),
                            BytecodeLength: bytes.len(),
                        },
                        ..Default::default()
                    })
                    .map(|pso| Pipeline { root, pso })
            })
    };
    let made = match made {
        Ok(pipeline) => Arc::new(pipeline),
        Err(error) => {
            tracing::warn!(%error, "rectangle expansion pipeline refused");
            return None;
        }
    };
    pipelines.insert(key, made.clone());
    Some(made)
}

fn shader(layout: &Layout) -> String {
    let mut source = String::from(HELPERS);
    let stride = layout.stride;
    writeln!(source, "uint output_byte(uint position) {{").unwrap();
    writeln!(source, "uint vertex = (position / {stride}u) % 6u;").unwrap();
    writeln!(source, "uint within = position % {stride}u;").unwrap();
    writeln!(
        source,
        "uint base = (position / {}u) * {}u;",
        u64::from(stride) * 6,
        u64::from(stride) * 3
    )
    .unwrap();
    source.push_str(
        "uint original = vertex == 3u || vertex == 5u ? 0u : vertex == 4u ? 2u : vertex;\n",
    );
    writeln!(
        source,
        "uint result = source_byte(base + original * {stride}u + within);"
    )
    .unwrap();
    source.push_str("if (vertex == 5u) {\n");
    for &(offset, format) in &layout.elements {
        let Some((components, kind)) = super::components(format) else {
            continue;
        };
        let bytes = kind.bytes() as u32;
        let components = (components as u32).min(stride.saturating_sub(offset) / bytes);
        if components == 0 {
            continue;
        }
        let kind = match kind {
            super::Kind::F32 => 0,
            super::Kind::F16 => 1,
            super::Kind::Unorm8 => 2,
            super::Kind::Unorm16 => 3,
        };
        let end = offset + components * bytes;
        writeln!(source, "if (within >= {offset}u && within < {end}u) {{").unwrap();
        writeln!(
            source,
            "uint at = {offset}u + ((within - {offset}u) / {bytes}u) * {bytes}u;"
        )
        .unwrap();
        writeln!(
            source,
            "precise float a = source_value(base + at, {kind}u);"
        )
        .unwrap();
        writeln!(
            source,
            "precise float b = source_value(base + {}u + at, {kind}u);",
            u64::from(stride) * 2
        )
        .unwrap();
        writeln!(
            source,
            "precise float c = source_value(base + {stride}u + at, {kind}u);"
        )
        .unwrap();
        source.push_str("precise float sum = a + b;\nprecise float value = sum - c;\n");
        writeln!(source, "uint bits = encode_value(value, {kind}u);").unwrap();
        source.push_str("result = (bits >> ((within - at) * 8u)) & 255u;\n}\n");
    }
    source.push_str("}\nreturn result;\n}\n");
    source.push_str(ENTRY);
    source
}

const HELPERS: &str = r#"
cbuffer Parameters : register(b0) { uint output_bytes; uint row_words; };
ByteAddressBuffer vertices : register(t0);
RWByteAddressBuffer expanded : register(u0);

uint source_byte(uint at) {
    return (vertices.Load(at & ~3u) >> ((at & 3u) * 8u)) & 255u;
}
uint source_bits(uint at, uint bytes) {
    uint result = 0u;
    [unroll] for (uint i = 0u; i < 4u; ++i) {
        if (i < bytes) result |= source_byte(at + i) << (i * 8u);
    }
    return result;
}
float decode_half(uint value) {
    uint sign = (value >> 15u) << 31u;
    uint exponent = (value >> 10u) & 31u;
    uint mantissa = value & 1023u;
    if (exponent == 0u) {
        if (mantissa == 0u) return asfloat(sign);
        precise float scaled = (float)mantissa * 0.000000059604644775390625;
        return sign != 0u ? -scaled : scaled;
    }
    if (exponent == 31u) return asfloat(sign | 0x7f800000u | (mantissa << 13u));
    return asfloat(sign | ((exponent + 112u) << 23u) | (mantissa << 13u));
}
uint encode_half(float value) {
    uint bits = asuint(value);
    uint sign = (bits >> 16u) & 0x8000u;
    int exponent = (int)((bits >> 23u) & 255u) - 112;
    uint mantissa = bits & 0x7fffffu;
    if (exponent >= 31) return sign | 0x7c00u;
    if (exponent <= 0) return sign;
    // Preserve the existing conversion's OR/carry and underflow behavior.
    return sign | ((uint)exponent << 10u) | ((mantissa + 0x1000u) >> 13u);
}
float source_value(uint at, uint kind) {
    if (kind == 0u) return asfloat(source_bits(at, 4u));
    if (kind == 1u) return decode_half(source_bits(at, 2u));
    if (kind == 2u) return (float)source_byte(at) / 255.0;
    return (float)source_bits(at, 2u) / 65535.0;
}
uint encode_value(float value, uint kind) {
    if (kind == 0u) return asuint(value);
    if (kind == 1u) return encode_half(value);
    if (isnan(value)) return 0u;
    precise float bounded = clamp(value, 0.0, 1.0);
    precise float scaled = bounded * (kind == 2u ? 255.0 : 65535.0);
    uint whole = (uint)scaled;
    precise float fractional = scaled - (float)whole;
    // Rust's positive rounding is ties-away; HLSL round is ties-even.
    // Adding 0.5 first would also misround the float just below a tie.
    return whole + (fractional >= 0.5 ? 1u : 0u);
}
"#;

const ENTRY: &str = r#"
[RootSignature("RootConstants(num32BitConstants=2, b0), SRV(t0), UAV(u0)")]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint word = id.x + id.y * row_words;
    uint words = output_bytes / 4u + (output_bytes % 4u != 0u ? 1u : 0u);
    if (word >= words) return;
    uint at = word * 4u;
    uint value = 0u;
    [unroll] for (uint i = 0u; i < 4u; ++i) {
        if (at + i < output_bytes) value |= output_byte(at + i) << (i * 8u);
    }
    expanded.Store(at, value);
}
"#;
