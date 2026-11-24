//! Authored shaders and resources only: verify that the translated PSO
//! state and title command-list setter filter the depth already stored.

use super::*;
use crate::object::{self, This};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::core::{PCSTR, s};

#[test]
fn depth_bounds_stream_state_survives_and_changes_cache_identity() {
    let mut g = GraphicsState::default();
    let disabled = g.describe();
    let extended = Sub(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1,
        D3D12_DEPTH_STENCIL_DESC1 {
            DepthEnable: g.depth.DepthEnable,
            DepthWriteMask: g.depth.DepthWriteMask,
            DepthFunc: g.depth.DepthFunc,
            StencilReadMask: g.depth.StencilReadMask,
            StencilWriteMask: g.depth.StencilWriteMask,
            FrontFace: g.depth.FrontFace,
            BackFace: g.depth.BackFace,
            DepthBoundsTestEnable: true.into(),
            ..Default::default()
        },
    );
    // SAFETY: the synthetic subobject is complete and lives through read.
    unsafe { g.read(21, (&raw const extended).cast()) };
    assert!(g.depth_bounds);
    assert!(g.depth_stencil1().DepthBoundsTestEnable.as_bool());
    assert_ne!(disabled, g.describe());

    // A legacy descriptor expresses disabled bounds; reusing state must
    // not retain the earlier extended descriptor's enabled flag.
    let legacy = Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL, g.depth);
    // SAFETY: a complete legacy subobject with the same base depth state.
    unsafe { g.read(11, (&raw const legacy).cast()) };
    assert!(!g.depth_bounds);
    assert_eq!(disabled, g.describe());
}

const SOURCE: &str = r#"
float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0.75, 1);
}
float4 ps() : SV_Target { return float4(1, 1, 1, 1); }
"#;

fn compile(entry: PCSTR, target: PCSTR) -> ID3DBlob {
    let mut code = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the authored source and static names outlive compilation.
    let result = unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("depth-bounds-regression"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let message = errors.map(|b| {
            // SAFETY: the compiler blob owns these bytes for the borrow.
            String::from_utf8_lossy(unsafe {
                std::slice::from_raw_parts(b.GetBufferPointer().cast(), b.GetBufferSize())
            })
            .into_owned()
        });
        panic!("authored test shader failed: {error}; {message:?}");
    }
    code.expect("compiled shader bytecode")
}

fn bytecode(blob: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    // SAFETY: the returned bytecode borrows a live compiler blob.
    unsafe {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: blob.GetBufferPointer(),
            BytecodeLength: blob.GetBufferSize(),
        }
    }
}

fn texture(
    device: &ID3D12Device,
    format: DXGI_FORMAT,
    flags: D3D12_RESOURCE_FLAGS,
    state: D3D12_RESOURCE_STATES,
) -> ID3D12Resource {
    let mut resource = None;
    // SAFETY: valid descriptions and our output pointer.
    unsafe {
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: 1,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Flags: flags,
                    ..Default::default()
                },
                state,
                None,
                &mut resource,
            )
            .unwrap();
    }
    resource.unwrap()
}

/// Keep title objects alive until every native command referring to their
/// list has completed, including when an assertion unwinds the test.
struct TitleList {
    list: This,
    allocator: This,
}

impl TitleList {
    fn new() -> Self {
        let null = std::ptr::null();
        let mut allocator = null;
        let mut list = null;
        // The device-child interface is shared by both object types.
        let iid = crate::resource::PAGEABLE_IIDS[1];
        assert_eq!(
            crate::list::create_allocator(null, 0, &iid, &mut allocator),
            object::S_OK
        );
        assert_eq!(
            crate::list::create_list(null, 0, 0, allocator, null, &iid, &mut list),
            object::S_OK
        );
        Self { list, allocator }
    }

    fn set_bounds(&self, min: f32, max: f32) {
        let slot = if crate::device::newer() { 0x230 } else { 0x220 };
        object::slot::<extern "system" fn(This, f32, f32)>(self.list, slot)(self.list, min, max);
    }

    fn reset(&self) {
        assert_eq!(
            object::slot::<extern "system" fn(This, This, This) -> i32>(self.list, 0x50)(
                self.list,
                self.allocator,
                std::ptr::null()
            ),
            object::S_OK
        );
    }
}

impl Drop for TitleList {
    fn drop(&mut self) {
        for this in [self.list, self.allocator] {
            object::slot::<extern "system" fn(This) -> u32>(this, 0x10)(this);
        }
    }
}

#[test]
#[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
fn depth_bounds_filter_stored_depth_and_reset() {
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let supported = host
        .depth_bounds_supported()
        .expect("OPTIONS2 capability query must succeed");
    eprintln!("VICTORY_DEPTH_BOUNDS_CAPABILITY supported={supported}");
    if !supported {
        eprintln!("VICTORY_DEPTH_BOUNDS_RESULT=unsupported cases=0");
        return;
    }
    let vs = compile(s!("vs"), s!("vs_5_0"));
    let ps = compile(s!("ps"), s!("ps_5_0"));
    let (root, root_blob) = root_signature(&host.device, &[], &[], false).unwrap();
    let mut g = GraphicsState::default();
    g.depth.DepthEnable = false.into();
    g.depth.DepthWriteMask = D3D12_DEPTH_WRITE_MASK_ZERO;
    g.depth_format = DXGI_FORMAT_D32_FLOAT;
    g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
    g.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
    g.targets.NumRenderTargets = 1;
    g.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
    let pipelines: Vec<ID3D12PipelineState> = [false, true]
        .into_iter()
        .map(|enabled| {
            g.depth_bounds = enabled;
            let name = super::super::library::name(
                "graphics",
                &root_blob,
                &[&code_bytes(&bytecode(&vs)), &code_bytes(&bytecode(&ps))],
                &g.describe(),
            );
            graphics_pipeline(&host.device, &root, bytecode(&vs), bytecode(&ps), &g, &name).unwrap()
        })
        .collect();
    let color = texture(
        &host.device,
        DXGI_FORMAT_R8G8B8A8_UNORM,
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        D3D12_RESOURCE_STATE_RENDER_TARGET,
    );
    let depth = texture(
        &host.device,
        DXGI_FORMAT_D32_FLOAT,
        D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
        D3D12_RESOURCE_STATE_DEPTH_WRITE,
    );
    let rtv = host.rtv(&color, None);
    let dsv = host.dsv(&depth, None);
    // (enabled, dynamic range change, stored depth, expected samples).
    // The shader emits depth 0.75: a [0.5, 0.5] pass proves the test uses
    // stored depth instead of the shader's incoming depth.
    let cases = [
        (true, None, 0.5, 1),
        (true, Some((0.5, 0.5)), 0.5, 1),
        (true, Some((0.500_01, 1.0)), 0.5, 0),
        (true, Some((0.0, 0.499_99)), 0.5, 0),
        (true, Some((0.6, 0.4)), 0.5, 0),
        (false, None, 0.5, 1),
        (true, None, 0.5, 0),
        (true, Some((f32::NAN, 0.5)), 0.5, 1),
        (true, Some((0.5, f32::NAN)), 0.5, 0),
        (true, Some((0.0, f32::NAN)), 0.0, 1),
        (true, Some((-1.0, 2.0)), 0.5, 1),
        (true, Some((0.25, 0.75)), 0.5, 1),
        (true, Some((0.75, 1.0)), 0.5, 0),
    ];
    let count = cases.len() as u32 + 1;
    let mut query: Option<ID3D12QueryHeap> = None;
    let mut readback: Option<ID3D12Resource> = None;
    // SAFETY: complete descriptions and our output pointers.
    unsafe {
        host.device
            .CreateQueryHeap(
                &D3D12_QUERY_HEAP_DESC {
                    Type: D3D12_QUERY_HEAP_TYPE_OCCLUSION,
                    Count: count,
                    NodeMask: 0,
                },
                &mut query,
            )
            .unwrap();
        host.device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_READBACK,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: u64::from(count) * 8,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )
            .unwrap();
    }
    let (query, readback) = (query.unwrap(), readback.unwrap());
    let title = TitleList::new();
    let list = crate::list::host_list(title.list).unwrap();
    let bind = || {
        // SAFETY: the list is open and all referenced objects remain live.
        unsafe {
            list.list.SetGraphicsRootSignature(&root);
            list.list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            list.list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: 1.0,
                Height: 1.0,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            list.list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            }]);
            list.list
                .OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
        }
    };
    let draw = |index: u32, enabled: bool, stored: f32| {
        // SAFETY: live descriptors, PSO and query index inside the heap.
        unsafe {
            list.list.SetPipelineState(&pipelines[usize::from(enabled)]);
            list.list
                .ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, stored, 0, None);
            list.list
                .BeginQuery(&query, D3D12_QUERY_TYPE_OCCLUSION, index);
            list.list.DrawInstanced(3, 1, 0, 0);
            list.list
                .EndQuery(&query, D3D12_QUERY_TYPE_OCCLUSION, index);
        }
    };
    bind();
    for (index, &(enabled, range, stored, _)) in cases.iter().enumerate() {
        if let Some((min, max)) = range {
            title.set_bounds(min, max);
        }
        draw(index as u32, enabled, stored);
    }
    // SAFETY: the query interval and destination range fit their objects.
    unsafe {
        list.list.ResolveQueryData(
            &query,
            D3D12_QUERY_TYPE_OCCLUSION,
            0,
            count - 1,
            &readback,
            0,
        );
    }
    list.close();
    host.execute(&[list]);
    title.reset();
    bind();
    draw(count - 1, true, 0.5);
    // The old list's rejecting range must not survive Reset. Its new
    // default [0, 1] passes without a new OMSetDepthBounds call.
    // SAFETY: the last query and eight-byte destination fit their objects.
    unsafe {
        list.list.ResolveQueryData(
            &query,
            D3D12_QUERY_TYPE_OCCLUSION,
            count - 1,
            1,
            &readback,
            u64::from(count - 1) * 8,
        );
    }
    list.close();
    host.execute(&[list]);
    let mut mapped = std::ptr::null_mut();
    // SAFETY: both submissions completed; Map exposes the full readback.
    let samples = unsafe {
        readback
            .Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: count as usize * 8,
                }),
                Some(&mut mapped),
            )
            .unwrap();
        let samples = std::slice::from_raw_parts(mapped.cast::<u64>(), count as usize).to_vec();
        readback.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        samples
    };
    for (index, &(_, range, stored, expected)) in cases.iter().enumerate() {
        assert_eq!(
            samples[index], expected,
            "case {index}: range={range:?}, stored={stored}"
        );
    }
    assert_eq!(samples[count as usize - 1], 1, "reset must restore [0, 1]");
    eprintln!("VICTORY_DEPTH_BOUNDS_RESULT=executed cases={count}");
}
