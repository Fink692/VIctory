//! Authored shaders and resources only: verify that the translated PSO
//! state and title command-list setter filter the depth already stored.

use super::*;
use crate::object::{self, This};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::core::{Interface, PCSTR, s};

type Messages = std::sync::Mutex<Vec<(D3D12_MESSAGE_ID, D3D12_MESSAGE_SEVERITY, String)>>;

/// Observe validation even when Host::execute drains the native queue.
/// Inverted ranges have a specified result and an intentional diagnostic;
/// every other native error invalidates the measurement.
struct Validation {
    queue: Option<ID3D12InfoQueue1>,
    messages: Box<Messages>,
    cookie: u32,
}

impl Validation {
    fn new(device: &ID3D12Device) -> Self {
        unsafe extern "system" fn message(
            _: D3D12_MESSAGE_CATEGORY,
            severity: D3D12_MESSAGE_SEVERITY,
            id: D3D12_MESSAGE_ID,
            description: PCSTR,
            context: *mut std::ffi::c_void,
        ) {
            if severity.0 <= D3D12_MESSAGE_SEVERITY_ERROR.0
                || id == D3D12_MESSAGE_ID_SETDEPTHBOUNDS_INVALIDARGS
            {
                // SAFETY: the boxed context outlives registration and the
                // callback's description is valid for this invocation.
                let (messages, text) = unsafe {
                    (
                        &*context.cast::<Messages>(),
                        std::ffi::CStr::from_ptr(description.0.cast())
                            .to_string_lossy()
                            .into_owned(),
                    )
                };
                messages
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((id, severity, text));
            }
        }
        let mut validation = Self {
            queue: None,
            messages: Box::default(),
            cookie: 0,
        };
        if cfg!(feature = "diagnostics")
            && std::env::var("VICTORY_D3D12_DEBUG").is_ok_and(|v| v == "1")
        {
            let queue = device
                .cast::<ID3D12InfoQueue1>()
                .expect("the requested debug layer must support message capture");
            // SAFETY: a stable boxed context, unregistered before release.
            // The callback records messages without calling back into D3D.
            unsafe {
                queue
                    .RegisterMessageCallback(
                        Some(message),
                        D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                        (&raw mut *validation.messages).cast(),
                        &mut validation.cookie,
                    )
                    .unwrap();
            }
            validation.queue = Some(queue);
        }
        validation
    }

    fn check(&self, phase: &str, expected_inversions: usize) {
        if self.queue.is_none() {
            return;
        }
        let messages =
            std::mem::take(&mut *self.messages.lock().unwrap_or_else(|e| e.into_inner()));
        let mut inversions = 0;
        let mut unexpected = Vec::new();
        for (id, severity, text) in messages {
            eprintln!(
                "VICTORY_DEPTH_BOUNDS_VALIDATION_MESSAGE phase={phase} id={} severity={} {text}",
                id.0, severity.0
            );
            if id == D3D12_MESSAGE_ID_SETDEPTHBOUNDS_INVALIDARGS
                && matches!(
                    severity,
                    D3D12_MESSAGE_SEVERITY_ERROR | D3D12_MESSAGE_SEVERITY_WARNING
                )
            {
                inversions += 1;
            } else {
                unexpected.push(text);
            }
        }
        assert!(
            unexpected.is_empty(),
            "{phase}: native validation errors: {unexpected:?}"
        );
        assert_eq!(
            inversions, expected_inversions,
            "{phase}: inverted-range diagnostics"
        );
    }
}

impl Drop for Validation {
    fn drop(&mut self) {
        if let Some(queue) = &self.queue {
            // SAFETY: the registration and its context are still live.
            if let Err(error) = unsafe { queue.UnregisterMessageCallback(self.cookie) } {
                eprintln!("could not unregister depth validation callback: {error}");
                // Preserve the callback's context on an unexpected failure.
                Box::leak(std::mem::take(&mut self.messages));
            }
        }
    }
}

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

fn read_samples(readback: &ID3D12Resource, count: usize) -> Vec<u64> {
    let mut mapped = std::ptr::null_mut();
    // SAFETY: the caller waited for the submission; the readback contains
    // at least count query results and remains mapped while copied.
    unsafe {
        readback
            .Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: count * 8,
                }),
                Some(&mut mapped),
            )
            .unwrap();
        let samples = std::slice::from_raw_parts(mapped.cast::<u64>(), count).to_vec();
        readback.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        samples
    }
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
    let validation = Validation::new(&host.device);
    assert!(
        host.log_messages().is_none(),
        "device initialization produced native errors"
    );
    let vs = compile(s!("vs"), s!("vs_5_0"));
    let ps = compile(s!("ps"), s!("ps_5_0"));
    let (root, root_blob) = root_signature(&host.device, &[], &[], false).unwrap();
    // The translated root signature enables both directly indexed heaps.
    // D3D12 requires them to be bound before that root signature even
    // though these authored shaders do not read any heap descriptors.
    let heaps = [
        D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER,
    ]
    .map(|kind| {
        // SAFETY: a nonempty shader-visible heap of a supported type.
        Some(unsafe {
            host.device
                .CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    ..Default::default()
                })
                .unwrap()
        })
    });
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
            list.list.SetDescriptorHeaps(&heaps);
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
        validation.check("bind", 0);
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
            validation.check("before-bounds", 0);
            title.set_bounds(min, max);
            // The depth-bounds specification requires inverted intervals
            // to reach the driver and reject samples despite a diagnostic.
            // Scope the sole permitted message to this exact setter call.
            let value = |x: f32| if x.is_nan() { 0.0 } else { x };
            validation.check("bounds", usize::from(value(min) > value(max)));
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
    validation.check("initial-close", 0);
    host.execute(&[list]);
    validation.check("initial-execute", 0);
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
    validation.check("reset-close", 0);
    host.execute(&[list]);
    validation.check("reset-execute", 0);
    let samples = read_samples(&readback, count as usize);
    if std::env::var("VICTORY_DEPTH_BOUNDS_TRACE").is_ok_and(|v| v == "1") {
        for (index, &(enabled, range, stored, expected)) in cases.iter().enumerate() {
            eprintln!(
                "VICTORY_DEPTH_BOUNDS_CASE index={index} enabled={enabled} range={range:?} stored={stored} expected={expected} actual={}",
                samples[index]
            );
        }
        eprintln!(
            "VICTORY_DEPTH_BOUNDS_CASE index={} reset=true expected=1 actual={}",
            count - 1,
            samples[count as usize - 1]
        );
        let failed = cases
            .iter()
            .enumerate()
            .any(|(index, case)| samples[index] != case.3)
            || samples[count as usize - 1] != 1;
        if failed {
            let native = list.list.cast::<ID3D12GraphicsCommandList1>().unwrap();
            // These failure diagnostics retain the original assertions.
            // Compare the title route with a direct native call, then
            // test whether PSO binding/clearing loses the dynamic state.
            // ALWAYS with writes disabled has the same passing depth
            // comparison as DepthEnable=false. LESS/GREATER controls
            // check that the stored depth and DSV actually participate.
            let controls: Vec<ID3D12PipelineState> = [
                (true, D3D12_COMPARISON_FUNC_ALWAYS),
                (false, D3D12_COMPARISON_FUNC_LESS),
                (false, D3D12_COMPARISON_FUNC_GREATER),
            ]
            .into_iter()
            .map(|(bounds, comparison)| {
                g.depth.DepthEnable = true.into();
                g.depth.DepthFunc = comparison;
                g.depth_bounds = bounds;
                let name = super::super::library::name(
                    "graphics",
                    &root_blob,
                    &[&code_bytes(&bytecode(&vs)), &code_bytes(&bytecode(&ps))],
                    &g.describe(),
                );
                graphics_pipeline(&host.device, &root, bytecode(&vs), bytecode(&ps), &g, &name)
                    .unwrap()
            })
            .collect();
            let probes = [
                ("title-before-state", false, false, &pipelines[1], 0),
                ("native-before-state", true, false, &pipelines[1], 0),
                ("title-after-state", false, true, &pipelines[1], 0),
                ("native-after-state", true, true, &pipelines[1], 0),
                ("native-depth-always", true, true, &controls[0], 0),
                ("native-depth-less", true, true, &controls[1], 0),
                ("native-depth-greater", true, true, &controls[2], 1),
            ];
            for (label, direct, after_state, pipeline, expected) in probes {
                title.reset();
                bind();
                let set = || {
                    if direct {
                        // SAFETY: a fresh open list with advertised support.
                        unsafe { native.OMSetDepthBounds(0.75, 1.0) };
                    } else {
                        title.set_bounds(0.75, 1.0);
                    }
                };
                if !after_state {
                    set();
                }
                // SAFETY: the PSO and DSV remain live for the submission.
                unsafe {
                    list.list.SetPipelineState(pipeline);
                    list.list
                        .ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 0.5, 0, None);
                }
                if after_state {
                    set();
                }
                // SAFETY: reuse only the first query after prior work has
                // completed; the original results are already copied.
                unsafe {
                    list.list.BeginQuery(&query, D3D12_QUERY_TYPE_OCCLUSION, 0);
                    list.list.DrawInstanced(3, 1, 0, 0);
                    list.list.EndQuery(&query, D3D12_QUERY_TYPE_OCCLUSION, 0);
                    list.list.ResolveQueryData(
                        &query,
                        D3D12_QUERY_TYPE_OCCLUSION,
                        0,
                        1,
                        &readback,
                        0,
                    );
                }
                list.close();
                validation.check("probe-close", 0);
                host.execute(&[list]);
                validation.check("probe-execute", 0);
                let actual = read_samples(&readback, 1)[0];
                eprintln!(
                    "VICTORY_DEPTH_BOUNDS_PROBE label={label} range=[0.75,1] stored=0.5 expected={expected} actual={actual}"
                );
            }
        }
    }
    validation.check("complete", 0);
    if validation.queue.is_some() {
        eprintln!(
            "VICTORY_DEPTH_BOUNDS_VALIDATION=checked expected_inversions=2 unexpected_errors=0"
        );
    }
    for (index, &(_, range, stored, expected)) in cases.iter().enumerate() {
        assert_eq!(
            samples[index], expected,
            "case {index}: range={range:?}, stored={stored}"
        );
    }
    assert_eq!(samples[count as usize - 1], 1, "reset must restore [0, 1]");
    eprintln!("VICTORY_DEPTH_BOUNDS_RESULT=executed cases={count}");
}
