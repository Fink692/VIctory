//! Indirect execution's arguments, bounded before the GPU reads them. The
//! title's own compute passes write most of them; a pass we drop or
//! translate wrongly leaves whatever the memory held, and a draw of four
//! billion vertices or a dispatch past the host's limits runs until the
//! host resets the display driver. So every record is clamped on the GPU,
//! just before its execution, by a compute shader of ours (HLSL compiled by
//! the system's compiler; a 5.1 shader with its root signature inside).
//!
//! Each list also has a budget (2^27 vertex instances or threads, groups
//! counted as 64 threads, mesh groups as 128): records past it run empty,
//! so a list of thousands of garbage records cannot run for seconds
//! either.
//!
//! shortcut: the bounds (2^24 vertices, 2^26 vertex instances, 65,535
//! groups per dimension and 2^20 per dispatch, one-dimensional mesh
//! dispatches, the budget) cut legitimate work beyond them; raise them if
//! the title needs more.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=4, b0), UAV(u0), UAV(u1), UAV(u2)"
cbuffer Record : register(b0) { uint kind; uint stride; uint records; uint has_counter; };
RWByteAddressBuffer args : register(u0);
// The list's work so far (vertex instances, threads): past the budget,
// records run empty.
RWByteAddressBuffer budget : register(u1);
RWByteAddressBuffer counter : register(u2);
static const uint BUDGET = 1u << 27;
// Groups of one dispatch; more run empty.
static const uint GROUPS = 1u << 20;
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint count = records;
    if (has_counter != 0) count = min(count, counter.Load(0));
    if (id.x >= count) return;
    uint at = id.x * stride;
    uint cost;
    if (kind <= 1) {
        // Draws: vertex or index count, then instances.
        uint count = min(args.Load(at), 1u << 24);
        uint instances = min(args.Load(at + 4), (1u << 26) / max(count, 1u));
        cost = count * instances;
        args.Store2(at, uint2(count, instances));
    } else if (kind == 2) {
        uint3 g = min(args.Load3(at), uint3(65535, 65535, 65535));
        // x * y fits 32 bits; the whole count may not.
        if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
            g = uint3(0, 0, 0);
        }
        cost = g.x * g.y * g.z * 64;
        args.Store3(at, g);
    } else {
        // Mesh dispatches: the console's are one-dimensional.
        uint3 g = args.Load3(at);
        g = uint3(min(g.x, 65535u), min(g.y, 1u), min(g.z, 1u));
        cost = g.x * g.y * g.z * 128;
        args.Store3(at, g);
    }
    // Saturating reservation: a wrapping InterlockedAdd eventually
    // reopens an exhausted budget when enough invalid records arrive.
    uint used;
    budget.InterlockedAdd(0, 0, used);
    bool accepted = false;
    [allow_uav_condition]
    while (used < BUDGET) {
        bool fits = cost <= BUDGET - used;
        uint next = fits ? used + cost : BUDGET;
        uint observed;
        budget.InterlockedCompareExchange(0, used, next, observed);
        if (observed == used) {
            accepted = fits;
            break;
        }
        used = observed;
    }
    if (!accepted) {
        // Draws keep their layout with no vertices; groups become zero.
        args.Store(at, 0);
    }
}
"#;

struct Clamp {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

/// Bytes the clamp and ExecuteIndirect will read. Validate before looking
/// up/allocating the shadow buffer: malformed strides or counts can
/// otherwise request enormous allocations before the clamp rejects them.
pub(crate) fn argument_bytes(kind: u32, stride: u32, records: u32) -> Option<u64> {
    let minimum = match kind {
        0 => 16,
        1 => 20,
        2 | 10 => 12,
        _ => return None,
    };
    let bytes = u64::from(stride) * u64::from(records);
    (stride.is_multiple_of(4)
        && stride >= minimum
        && records > 0
        && records.div_ceil(64) <= 65535
        && bytes <= u64::from(u32::MAX))
    .then_some(bytes)
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Clamp {}
// SAFETY: as above.
unsafe impl Sync for Clamp {}

fn clamp() -> Option<&'static Clamp> {
    static CLAMP: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    CLAMP
        .get_or_init(|| match make() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no argument clamp: indirect execution dropped");
                None
            }
        })
        .as_ref()
}

fn make() -> windows::core::Result<Clamp> {
    let device = &host().ok_or_else(windows::core::Error::empty)?.device;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("clamp"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(
                    errors.GetBufferPointer().cast::<u8>(),
                    errors.GetBufferSize(),
                )
            };
            tracing::warn!("argument clamp: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes; the root signature is inside the shader.
    unsafe {
        let bytes =
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: bytes.as_ptr().cast(),
                BytecodeLength: bytes.len(),
            },
            ..Default::default()
        })?;
        Ok(Clamp { root, pso })
    }
}

/// Records, in `list`, the clamp of `records` records of `kind` (PC
/// argument numbering) `stride` bytes apart at host address `gpu` in
/// `buffer`, and leaves the buffer ready to be read as arguments. Replaces
/// the list's compute root signature and pipeline (callers bind theirs
/// after). False if the clamp is not available.
pub(crate) fn bound(
    list: &HostList,
    buffer: &ID3D12Resource,
    gpu: u64,
    (kind, stride, records): (u32, u32, u32),
    counter: Option<(&ID3D12Resource, u64)>,
) -> bool {
    let (Some(c), Some(budget)) = (clamp(), list.budget()) else {
        return false;
    };
    if !gpu.is_multiple_of(4)
        || argument_bytes(kind, stride, records).is_none()
        || counter.is_some_and(|(_, at)| !at.is_multiple_of(4))
    {
        return false;
    }
    let l = &list.list;
    let constants = [kind, stride, records, u32::from(counter.is_some())];
    list.buffer_state(buffer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    if let Some((count, _)) = counter {
        list.buffer_state(count, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    }
    // SAFETY: an open list; our root signature, pipeline and a live buffer
    // the caller's list holds.
    unsafe {
        // Earlier shader writes (the title's argument passes) first.
        l.ResourceBarrier(&[uav_barrier()]);
        l.SetComputeRootSignature(&c.root);
        l.SetPipelineState(&c.pso);
        l.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
        l.SetComputeRootUnorderedAccessView(1, gpu);
        l.SetComputeRootUnorderedAccessView(2, budget);
        l.SetComputeRootUnorderedAccessView(3, counter.map_or(budget, |(_, at)| at));
        l.Dispatch(records.div_ceil(64), 1, 1);
        l.ResourceBarrier(&[uav_barrier()]);
    }
    list.buffer_state(buffer, D3D12_RESOURCE_STATE_GENERIC_READ);
    if let Some((count, _)) = counter {
        list.buffer_state(count, D3D12_RESOURCE_STATE_GENERIC_READ);
    }
    true
}

/// After the execution: the buffer back to unordered access, where the
/// title's passes keep using it.
pub(crate) fn done(list: &HostList, buffer: &ID3D12Resource, counter: Option<&ID3D12Resource>) {
    list.buffer_state(buffer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    if let Some(count) = counter {
        list.buffer_state(count, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    }
}

fn uav_barrier() -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: std::mem::ManuallyDrop::new(None),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indirect_layout_rejects_truncated_records_and_dispatch_overflow() {
        assert_eq!(argument_bytes(0, 16, 3), Some(48));
        assert_eq!(argument_bytes(1, 20, 3), Some(60));
        assert_eq!(argument_bytes(2, 12, 3), Some(36));
        assert_eq!(argument_bytes(10, 12, 3), Some(36));
        assert_eq!(argument_bytes(1, 16, 3), None);
        assert_eq!(argument_bytes(0, 17, 3), None);
        assert_eq!(argument_bytes(0, 16, 65535 * 64 + 1), None);
        assert_eq!(argument_bytes(0, u32::MAX - 3, 2), None);
        assert_eq!(argument_bytes(99, 16, 1), None);
    }
    /// The clamp compiles with the system's compiler and the host takes it.
    #[test]
    fn argument_clamp_builds() {
        if super::host().is_none() {
            return; // no host GPU here
        }
        assert!(super::clamp().is_some());
    }

    /// Ninety-six maximum-cost records used to wrap the budget counter
    /// and reopen it. This executes the actual HLSL, including its atomics.
    #[test]
    #[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
    fn indirect_budget_cannot_wrap_and_ignores_inactive_records() {
        let host = host().expect("a D3D12 adapter is required");
        let records = 96u32;
        let (args, data) = host.shared_buffer(u64::from(records) * 16).unwrap();
        let (counter, count_data) = host.shared_buffer(4).unwrap();
        // SAFETY: the two live, mapped buffers have the requested sizes.
        let words =
            unsafe { std::slice::from_raw_parts_mut(data.cast::<u32>(), records as usize * 4) };
        for record in words.chunks_exact_mut(4) {
            record.copy_from_slice(&[1 << 24, 4, 0, 0]);
        }
        let list = host.new_list().unwrap();
        list.begin_buffer_bindings();
        list.bind_buffer(&args, false);
        list.bind_buffer(&args, false);
        assert!(list.finish_buffer_bindings(), "read-only aliases combine");
        list.begin_buffer_bindings();
        list.bind_buffer(&args, false);
        list.bind_buffer(&args, true);
        assert!(
            !list.finish_buffer_bindings(),
            "SRV/UAV aliases must be rejected"
        );
        // SAFETY: live buffers; address queries have no side effects.
        let (gpu, counter_gpu) =
            unsafe { (args.GetGPUVirtualAddress(), counter.GetGPUVirtualAddress()) };
        assert!(bound(&list, &args, gpu, (0, 16, records), None));
        done(&list, &args, None);
        list.close();
        host.execute(&[&list]);
        let cost: u64 = words
            .chunks_exact(4)
            .map(|r| u64::from(r[0]) * u64::from(r[1]))
            .sum();
        assert_eq!(cost, 1 << 27);

        list.reset();
        for record in words.chunks_exact_mut(4) {
            record.copy_from_slice(&[1 << 24, 4, 0, 0]);
        }
        // SAFETY: a writable mapped word, GPU idle.
        unsafe { count_data.cast::<u32>().write(1) };
        assert!(bound(
            &list,
            &args,
            gpu,
            (0, 16, records),
            Some((&counter, counter_gpu))
        ));
        done(&list, &args, Some(&counter));
        list.close();
        host.execute(&[&list]);
        // Only one record executes; inactive records must not consume the
        // budget or have their arguments overwritten.
        for record in words.chunks_exact(4) {
            assert_eq!(record, &[1 << 24, 4, 0, 0]);
        }
    }
}
