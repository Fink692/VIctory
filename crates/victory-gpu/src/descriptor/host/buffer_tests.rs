//! Authored shaders consuming the runtime's real mirrored structured,
//! raw and typed buffer descriptors. No title inputs or captured binaries.

use super::*;
use crate::draw::constant_tests::Validation;
use windows::Win32::Graphics::Direct3D::{Fxc::D3DCompile, ID3DBlob};
use windows::core::s;

const BYTES: usize = 256;

struct Memory(Box<[u32]>);

impl Memory {
    fn new(base: u32) -> Self {
        let result = Self((0..BYTES / 4).map(|i| base + i as u32).collect());
        crate::shadow::register(result.address() as usize, result.address(), BYTES as u64);
        result
    }

    fn address(&self) -> u64 {
        self.0.as_ptr() as u64
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        crate::shadow::unregister(self.address() as usize, self.address());
    }
}

struct Program {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Structured,
    Raw,
    Typed,
}

impl Program {
    fn new(device: &ID3D12Device, kind: Kind, stride: u32) -> Self {
        let (resources, operations) = match kind {
            Kind::Structured => (
                r#"
struct Element { uint words[WORDS]; };
StructuredBuffer<Element> source : register(t0);
RWStructuredBuffer<Element> target : register(u0);
"#
                .replace("WORDS", &(stride / 4).to_string()),
                r#"
    uint source_count, source_stride, target_count, target_stride;
    source.GetDimensions(source_count, source_stride);
    target.GetDimensions(target_count, target_stride);
    uint value = source[index].words[offset / 4];
    uint previous;
    InterlockedAdd(target[index].words[offset / 4], 3, previous);
    target[index + 1].words[offset / 4] = value;
    output.Store4(16, uint4(value, previous,
        source_count * source_stride, target_count * target_stride));
"#,
            ),
            Kind::Raw => (
                r#"
ByteAddressBuffer source : register(t0);
RWByteAddressBuffer target : register(u0);
"#
                .to_owned(),
                r#"
    uint source_bytes, target_bytes;
    source.GetDimensions(source_bytes);
    target.GetDimensions(target_bytes);
    uint value = source.Load(index * stride + offset);
    uint previous;
    target.InterlockedAdd(index * stride + offset, 3, previous);
    target.Store((index + 1) * stride + offset, value);
    output.Store4(16, uint4(value, previous, source_bytes, target_bytes));
"#,
            ),
            Kind::Typed => (
                "Buffer<uint4> source : register(t0);\nRWBuffer<uint4> target : register(u0);\n"
                    .to_owned(),
                "output.Store4(16, source[index]);\n",
            ),
        };
        const COMMON: &str = r#"
RWByteAddressBuffer output : register(u1);
cbuffer Parameters : register(b0) { uint stride; uint index; uint offset; };
[RootSignature("DescriptorTable(SRV(t0), UAV(u0)), UAV(u1), RootConstants(num32BitConstants=3, b0)")]
[numthreads(1, 1, 1)]
void main() {
"#;
        let source = [resources.as_str(), COMMON, operations, "}\n"].concat();
        let mut code = None;
        let mut errors: Option<ID3DBlob> = None;
        // SAFETY: authored source and static entry/profile names, valid outputs.
        let compiled = unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                s!("mirrored-buffer-fixture"),
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
        if let Err(error) = compiled {
            let detail = errors.map(|blob| {
                // SAFETY: the live compiler blob owns this byte range.
                String::from_utf8_lossy(unsafe {
                    std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
                })
                .into_owned()
            });
            panic!("buffer descriptor fixture: {kind:?}/{stride}: {error}; {detail:?}");
        }
        let code = code.unwrap();
        // SAFETY: native creation copies live authored code/descriptions.
        unsafe {
            let bytes =
                std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize());
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes).unwrap();
            let pso = device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: bytes.as_ptr().cast(),
                        BytecodeLength: bytes.len(),
                    },
                    ..Default::default()
                })
                .unwrap();
            Self { root, pso }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Case {
    kind: Kind,
    mapping: u32,
    offset: u64,
    width: u64,
    first: u64,
    count: u32,
    stride: u32,
    index: u32,
    field: u32,
    valid: bool,
}

fn view(memory: &Memory, case: Case, unordered: bool) -> View {
    let format = match case.kind {
        Kind::Structured => DXGI_FORMAT_UNKNOWN,
        Kind::Raw => DXGI_FORMAT_R32_TYPELESS,
        Kind::Typed => DXGI_FORMAT_R32G32B32A32_UINT,
    };
    let structure_stride = if case.kind == Kind::Structured {
        case.stride
    } else {
        0
    };
    let words: [u32; 10] = if unordered {
        let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    FirstElement: case.first,
                    NumElements: case.count,
                    StructureByteStride: structure_stride,
                    Flags: if case.kind == Kind::Raw {
                        D3D12_BUFFER_UAV_FLAG_RAW
                    } else {
                        D3D12_BUFFER_UAV_FLAG_NONE
                    },
                    ..Default::default()
                },
            },
        };
        // SAFETY: both layouts are exactly forty bytes, as in title descriptors.
        unsafe { std::mem::transmute::<D3D12_UNORDERED_ACCESS_VIEW_DESC, [u32; 10]>(desc) }
    } else {
        let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: case.mapping,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_SRV {
                    FirstElement: case.first,
                    NumElements: case.count,
                    StructureByteStride: structure_stride,
                    Flags: if case.kind == Kind::Raw {
                        D3D12_BUFFER_SRV_FLAG_RAW
                    } else {
                        D3D12_BUFFER_SRV_FLAG_NONE
                    },
                },
            },
        };
        // SAFETY: both layouts are exactly forty bytes.
        unsafe { std::mem::transmute::<D3D12_SHADER_RESOURCE_VIEW_DESC, [u32; 10]>(desc) }
    };
    let mut resource = [0u32; 14];
    resource[0] = 1;
    resource[4] = case.width as u32;
    resource[5] = (case.width >> 32) as u32;
    let mut placed = [0u32; 16];
    placed[..10].copy_from_slice(&words);
    let address = memory.address() + case.offset;
    placed[10] = address as u32;
    placed[11] = (address >> 32) as u32;
    View::Placed(unordered, resource, placed)
}

#[test]
#[ignore = "requires diagnostics, D3D12 and VICTORY_D3D12_DEBUG=1"]
fn mirrored_buffer_views_preserve_kinds_ranges_and_alignment() {
    crate::host::on_failure(|message| {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "VICTORY_BUFFER_VIEW_FAILURE {message}");
        std::process::abort();
    });
    let host = host().expect("a D3D12 adapter is required");
    let validation = Validation::new(&host.device);
    let programs: std::collections::HashMap<_, _> = [
        (Kind::Structured, 4),
        (Kind::Structured, 8),
        (Kind::Structured, 12),
        (Kind::Structured, 16),
        (Kind::Structured, 32),
        (Kind::Raw, 0),
        (Kind::Typed, 0),
    ]
    .into_iter()
    .map(|(kind, stride)| ((kind, stride), Program::new(&host.device, kind, stride)))
    .collect();
    let heap = HostHeap::new(false, 2).expect("native descriptor heap");
    let ordinary = Case {
        kind: Kind::Structured,
        mapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        offset: 0,
        width: BYTES as u64,
        first: 2,
        count: 4,
        stride: 16,
        index: 1,
        field: 4,
        valid: true,
    };
    let cases = [
        ordinary,
        Case {
            first: 4,
            count: 8,
            stride: 4,
            index: 2,
            field: 0,
            ..ordinary
        },
        Case {
            first: 2,
            count: 8,
            stride: 8,
            index: 2,
            field: 4,
            ..ordinary
        },
        Case {
            first: 4,
            count: 6,
            stride: 12,
            index: 1,
            field: 8,
            ..ordinary
        },
        Case {
            first: 1,
            count: 4,
            stride: 32,
            index: 0,
            field: 28,
            ..ordinary
        },
        Case {
            offset: 16,
            width: 240,
            first: 0,
            ..ordinary
        },
        Case {
            index: 3,
            field: 12,
            ..ordinary
        }, // last dword, next store is out of range
        Case {
            index: 4,
            field: 0,
            ..ordinary
        }, // read, atomic and store all out of range
        Case {
            first: 1,
            stride: 4,
            field: 0,
            ..ordinary
        }, // a valid structured view starting at byte 4
        Case {
            count: 0,
            valid: false,
            ..ordinary
        },
        Case {
            stride: 3,
            field: 0,
            valid: false,
            ..ordinary
        },
        Case {
            first: u64::MAX,
            valid: false,
            ..ordinary
        },
        Case {
            width: 64,
            first: 4,
            count: 2,
            valid: false,
            ..ordinary
        }, // smaller title resource in a larger shadow
        Case {
            count: u32::MAX,
            valid: false,
            ..ordinary
        },
        Case {
            offset: 8,
            width: 248,
            first: 0,
            valid: false,
            ..ordinary
        },
        Case {
            offset: 8,
            width: 248,
            first: 2,
            stride: 4,
            field: 0,
            ..ordinary
        }, // native offset need only be on a structured element
        Case {
            first: 2,
            stride: 4,
            field: 0,
            ..ordinary
        }, // valid structured start at byte 8
        Case {
            first: 3,
            stride: 4,
            field: 0,
            ..ordinary
        }, // valid structured start at byte 12
        Case {
            first: 1,
            stride: 8,
            field: 4,
            ..ordinary
        },
        Case {
            first: 1,
            stride: 12,
            field: 8,
            ..ordinary
        },
        Case {
            offset: 12,
            width: 244,
            first: 1,
            stride: 12,
            field: 8,
            ..ordinary
        }, // neither the resource nor view start needs to align to 16 bytes
        Case {
            kind: Kind::Raw,
            first: 4,
            count: 8,
            stride: 4,
            index: 2,
            field: 0,
            ..ordinary
        },
        Case {
            kind: Kind::Raw,
            offset: 16,
            width: 240,
            first: 0,
            count: 8,
            stride: 4,
            field: 0,
            ..ordinary
        },
        Case {
            kind: Kind::Raw,
            first: 1,
            stride: 4,
            field: 0,
            valid: false,
            ..ordinary
        }, // a raw view still requires 16-byte alignment
        Case {
            kind: Kind::Typed,
            first: 1,
            ..ordinary
        },
        Case {
            kind: Kind::Typed,
            // Z, X, forced 1, Y. Keep an asymmetric native mapping.
            mapping: 2 | (5 << 6) | (1 << 9) | (1 << 12),
            first: 1,
            ..ordinary
        },
    ];
    let case_count = cases.len();
    for (number, case) in cases.into_iter().enumerate() {
        let program_stride = if case.kind == Kind::Structured {
            if structured_stride(case.stride) {
                case.stride
            } else {
                4 // malformed structured descriptions get a valid null
            }
        } else {
            0
        };
        let program = &programs[&(case.kind, program_stride)];
        let source = Memory::new(0x1020_0000);
        let target = Memory::new(0x2040_0000);
        let mut expected_target = target.0.to_vec();
        let source_view = view(&source, case, false);
        let target_view = view(&target, case, true);
        heap.make_at(0, &source_view);
        heap.make_at(1, &target_view);
        let source_shadow = crate::shadow::find(source.address(), BYTES as u64).unwrap();
        let target_shadow = crate::shadow::find(target.address(), BYTES as u64).unwrap();
        let (output, data) = host.shared_buffer(48).unwrap();
        // SAFETY: newly allocated mapped memory, not submitted yet.
        unsafe { std::ptr::write_bytes(data, 0xa5, 48) };
        let list = host.new_list().unwrap();
        source_shadow.refresh(&list, source.address(), BYTES as u64);
        target_shadow.refresh(&list, target.address(), BYTES as u64);
        target_shadow.gpu_writes(&list, target.address(), BYTES as u64);
        list.bind_buffer(&source_shadow.buffer, false);
        list.bind_buffer(&target_shadow.buffer, true);
        list.bind_buffer(&output, true);
        list.keep(&heap.heap);
        list.keep(&program.root);
        list.keep(&program.pso);
        let parameters = [case.stride, case.index, case.field];
        // SAFETY: authored signature and matching live descriptors/resources.
        unsafe {
            list.list.SetDescriptorHeaps(&[Some(heap.heap.clone())]);
            list.list.SetComputeRootSignature(&program.root);
            list.list.SetPipelineState(&program.pso);
            list.list
                .SetComputeRootDescriptorTable(0, heap.heap.GetGPUDescriptorHandleForHeapStart());
            list.list
                .SetComputeRootUnorderedAccessView(1, output.GetGPUVirtualAddress());
            list.list
                .SetComputeRoot32BitConstants(2, 3, parameters.as_ptr().cast(), 0);
            list.list.Dispatch(1, 1, 1);
        }
        list.close();
        host.execute(&[&list]);
        // SAFETY: GPU completion was awaited and mappings remain live.
        let actual = unsafe { std::slice::from_raw_parts(data, 48) };
        assert_eq!(&actual[..16], &[0xa5; 16], "prefix {case:?}");
        assert_eq!(&actual[32..], &[0xa5; 16], "suffix {case:?}");
        let values: Vec<u32> = actual[16..32]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        let mut expected_value = 0;
        let mut expected_previous = 0;
        if case.kind == Kind::Typed {
            let first = case.offset + (case.first + u64::from(case.index)) * 16;
            let words = &source.0[first as usize / 4..first as usize / 4 + 4];
            let expected: Vec<u32> = (0..4)
                .map(|channel| match (case.mapping >> (channel * 3)) & 7 {
                    0..=3 => words[((case.mapping >> (channel * 3)) & 7) as usize],
                    4 => 0,
                    5 => 1,
                    _ => unreachable!("authored component mapping"),
                })
                .collect();
            assert_eq!(values, expected, "typed component mapping {case:?}");
        } else if case.valid {
            let bytes = case.count * case.stride;
            assert_eq!(&values[2..], &[bytes; 2], "view dimensions {case:?}");
            let base = case.offset + case.first * u64::from(case.stride);
            let at = case.index * case.stride + case.field;
            if at + 4 <= bytes {
                let word = (base + u64::from(at)) as usize / 4;
                expected_value = source.0[word];
                expected_previous = expected_target[word];
                expected_target[word] += 3;
            }
            if at + case.stride + 4 <= bytes {
                let word = (base + u64::from(at + case.stride)) as usize / 4;
                expected_target[word] = expected_value;
            }
        }
        if case.kind != Kind::Typed {
            assert_eq!(
                &values[..2],
                &[expected_value, expected_previous],
                "load/atomic {case:?}"
            );
        }
        assert_eq!(&*target.0, expected_target, "target and guards {case:?}");
        validation.check();
        println!("VICTORY_BUFFER_VIEW_CASE={} status=passed", number + 1);
    }
    println!("VICTORY_BUFFER_VIEW_RESULT=executed cases={case_count} validation_errors=0");
}

#[test]
fn buffer_ranges_keep_large_structured_views_and_reject_overflow() {
    // The old structured ABI counts records, so this valid window is
    // larger than 4 GiB. Converting it to raw would lose that addressing.
    let count = (1 << 28) + 1;
    let bytes = u64::from(count) * 16;
    assert!(buffer_view_ok(
        DXGI_FORMAT_UNKNOWN,
        1,
        count,
        16,
        false,
        bytes + 16
    ));
    assert_eq!(
        checked_buffer_range(0x1000, bytes + 16, 1, count, 16),
        Some((0x1010, bytes))
    );
    for first in 1..=3 {
        assert!(buffer_view_ok(DXGI_FORMAT_UNKNOWN, first, 4, 4, false, 64));
    }
    assert!(!buffer_view_ok(DXGI_FORMAT_UNKNOWN, 0, 4, 3, false, 64));
    assert!(!buffer_view_ok(DXGI_FORMAT_R32_TYPELESS, 1, 4, 0, true, 64));
    assert_eq!(checked_buffer_range(0x1000, 64, 4, 2, 16), None);
    assert_eq!(checked_buffer_range(0, u64::MAX, u64::MAX, 1, 16), None);
    assert_eq!(checked_buffer_range(u64::MAX - 1, 4, 0, 1, 4), None);
}
