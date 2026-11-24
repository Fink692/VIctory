//! Authored mixed byte/indexed buffer programs. Both addressing forms use
//! one structured host view, including its original record stride.

use super::{HostShader, lower};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const WORDS: u32 = 24;
const STRIDES: [u32; 4] = [4, 12, 16, 32];

#[derive(Clone, Copy, Debug)]
enum Op {
    Load(u8),
    Store(u8),
    Atomic(u8, bool),
    Packed(u8),
}

impl Op {
    fn uav(self) -> bool {
        !matches!(self, Self::Load(_) | Self::Packed(_))
    }
    fn code(self) -> u8 {
        let width = |n| match n {
            1 => 0,
            2 => 1,
            3 => 3,
            4 => 2,
            _ => unreachable!(),
        };
        match self {
            Self::Load(n) => 0x0c + width(n),
            Self::Store(n) => 0x1c + width(n),
            Self::Atomic(op, _) => op,
            Self::Packed(_) => unreachable!(),
        }
    }
}

const fn s_mov(dst: u8, src: u8) -> u32 {
    0xbe80_0300 | (dst as u32) << 16 | src as u32
}
const fn v_mov(dst: u8, src: u16) -> u32 {
    0x7e00_0200 | (dst as u32) << 17 | src as u32
}
const fn load_descriptor(dst: u8, base: u8) -> [u32; 2] {
    [
        0xf408_0000 | (dst as u32) << 6 | (base as u32) >> 1,
        125 << 25,
    ]
}

fn buffer(op: u8, data: u8, desc: u8, offset: u16, address: u32, glc: bool) -> [u32; 2] {
    [
        0xe000_0000
            | u32::from(op) << 18
            | u32::from(offset)
            | address << 12
            | u32::from(glc) << 14,
        (if address != 0 { 2 } else { 128 }) << 24
            | (u32::from(desc) >> 2) << 16
            | u32::from(data) << 8
            | u32::from(address == 1),
    ]
}

/// s0..s7: element, VGPR offset, SGPR offset, EXEC, four data dwords.
/// Tables s8 and s10 bind the tested view and the raw result UAV.
fn program(op: Op, stride: u32, immediate: u16) -> NativeProgram {
    let mut words = vec![s_mov(9, 128), s_mov(11, 128)];
    words.extend(load_descriptor(12, 8));
    words.extend(load_descriptor(16, 10));
    if op.uav() {
        // Initialization uses byte-addressed accesses to the SAME view as
        // the later indexed access, requiring the same structured handle kind.
        for k in 0..WORDS {
            words.extend([v_mov(10, 255), 1000 + k]);
            words.extend(buffer(0x1c, 10, 12, (4 * k) as u16, 0, false));
        }
    }
    words.extend([v_mov(0, 0), v_mov(1, 1)]);
    for k in 0..4u8 {
        words.push(v_mov(4 + k, u16::from(4 + k)));
        words.push(v_mov(12 + k, u16::from(4 + k)));
    }
    words.push(v_mov(11, 128));
    words.push(s_mov(126, 3));
    if let Op::Packed(format) = op {
        // tbuffer_load_format_xyzw v[4:7], v1, s[12:15], s2 offen format:F
        words.extend([
            0xe803_1000 | u32::from(format) << 19 | u32::from(immediate),
            0x0203_0401,
        ]);
    } else {
        // Before the indexed operation, read four consecutive dwords by
        // BYTE offset through the same descriptor, crossing records freely.
        words.extend(buffer(0x0e, 12, 12, immediate, 1, false));
        if matches!(op, Op::Atomic(..)) {
            // Byte-addressed atomic add of zero returns the same old value
            // as the linear read without changing the indexed case's data.
            words.extend(buffer(0x32, 11, 12, immediate, 1, true));
        }
        words.extend(buffer(
            op.code(),
            4,
            12,
            immediate,
            3,
            matches!(op, Op::Atomic(_, true)),
        ));
    }
    words.push(s_mov(126, 129));
    for k in 0..4u8 {
        words.extend(buffer(0x1c, 4 + k, 16, u16::from(k) * 4, 0, false));
    }
    if !matches!(op, Op::Packed(_)) {
        for k in 0..4u8 {
            words.extend(buffer(0x1c, 12 + k, 16, 16 + u16::from(k) * 4, 0, false));
        }
    }
    // A descriptor's dword2 is still its RECORD count, not raw byte length.
    words.push(v_mov(8, 14));
    words.extend(buffer(
        0x1c,
        8,
        16,
        if matches!(op, Op::Packed(_)) { 16 } else { 32 },
        0,
        false,
    ));
    if matches!(op, Op::Atomic(..)) {
        words.extend(buffer(0x1c, 11, 16, 36, 0, false));
    }
    words.push(0xbf81_0000);
    let table = |user_data, slot| RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot,
    };
    let used = |user_data, slot, stride, kind| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride,
        dwords: 4,
        user_data,
        slot,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 8,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            table(8, 1),
            table(10, 2),
        ],
        used: vec![
            used(
                8,
                1,
                stride,
                if op.uav() {
                    ResourceType::Uav
                } else {
                    ResourceType::Srv
                },
            ),
            used(10, 2, 0, ResourceType::Uav),
        ],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 11 << 1,
            threads: [1, 1, 1],
        }),
        code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
    }
}

fn build(op: Op, stride: u32, immediate: u16) -> Result<HostShader, ShaderError> {
    let native = program(op, stride, immediate);
    let p = ir::build(&native.code, native.wave64, native.vgprs)?;
    let st = structure::structure(&p)?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
    let io = io::raise(&p, &native)?;
    let l = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: vec![],
        outputs: vec![],
        prims: vec![],
        mesh_stride: 0,
    })?;
    Ok(HostShader {
        stage: HwStage::Cs,
        dxil: l.dxil,
        user_data: l.user_data,
        samplers: l.samplers,
        static_samplers: vec![],
        root_cbvs: l.root_cbvs,
        root_srvs: l.root_srvs,
    })
}

#[test]
fn indexed_buffer_operations_translate_without_int64() {
    let ops = (1..=4).flat_map(|n| [Op::Load(n), Op::Store(n)]).chain(
        (0x30..=0x3b)
            .filter(|op| !matches!(op, 0x33 | 0x34))
            .flat_map(|op| [Op::Atomic(op, false), Op::Atomic(op, true)]),
    );
    for op in ops {
        for stride in STRIDES {
            let shader = build(op, stride, 4).unwrap_or_else(|e| panic!("{op:?}/{stride}: {e}"));
            let c = crate::Container::parse(&shader.dxil).unwrap();
            let flags = c.part(crate::FourCC::SFI0).unwrap();
            assert_eq!(
                u64::from_le_bytes(flags.data.try_into().unwrap()) & 0x8000,
                0
            );
            if let Some(exe) = std::env::var_os("VICTORY_DXV") {
                super::atomic_tests::validate_dxv(&exe, &shader.dxil);
            }
        }
    }
    for stride in [0, 1, 2, 3, 5, 6, 7] {
        let e = build(Op::Load(4), stride, 0).unwrap_err();
        assert!(
            e.to_string()
                .contains("structured buffer of unknown stride"),
            "{e}"
        );
    }
}

#[test]
fn packed_buffer_formats_translate_and_validate() {
    for (format, stride) in (30..=49)
        .flat_map(|format| [0, 4, 8, 12].map(move |stride| (format, stride)))
        .chain(
            [64, 74, 77]
                .into_iter()
                .flat_map(|format| [4, 8, 12].map(move |stride| (format, stride))),
        )
    {
        let shader = build(Op::Packed(format), stride, 0).unwrap();
        if let Some(exe) = std::env::var_os("VICTORY_DXV") {
            super::atomic_tests::validate_dxv(&exe, &shader.dxil);
        }
    }
}

/// Expected channel bits from the published packed layouts. Float values
/// are computed from exponent/mantissa, independently of the half widening
/// used by the emitter. Integer outputs retain their raw signed/unsigned bits.
#[cfg(windows)]
fn packed_channels(format: u8, word: u32) -> ([u32; 4], bool) {
    let (widths, numeric): (&[u32], u8) = match format {
        30..=36 => (&[11, 11, 10], format - 30),
        37..=43 => (&[10, 11, 11], format - 37),
        44..=49 => (&[2, 10, 10, 10], format - 44),
        _ => unreachable!(),
    };
    let floating = !matches!(numeric, 4 | 5);
    let mut result = [0, 0, 0, if floating { 1.0f32.to_bits() } else { 1 }];
    let mut offset = 0;
    for (channel, &width) in widths.iter().enumerate() {
        let maximum = (1 << width) - 1;
        let field = word >> offset & maximum;
        offset += width;
        let signed = ((field << (32 - width)) as i32) >> (32 - width);
        result[channel] = match numeric {
            0 => ((f64::from(field) / f64::from(maximum)) as f32).to_bits(),
            1 => {
                ((f64::from(signed) / f64::from((1 << (width - 1)) - 1)).max(-1.0) as f32).to_bits()
            }
            2 => (field as f32).to_bits(),
            3 => (signed as f32).to_bits(),
            4 => field,
            5 => signed as u32,
            6 => {
                let mantissa_bits = width - 5;
                let exponent = field >> mantissa_bits;
                let fraction = field & ((1 << mantissa_bits) - 1);
                let value = match exponent {
                    0 => fraction as f32 * 2f32.powi(-14 - mantissa_bits as i32),
                    31 if fraction == 0 => f32::INFINITY,
                    31 => f32::NAN,
                    _ => {
                        (1.0 + fraction as f32 / (1 << mantissa_bits) as f32)
                            * 2f32.powi(exponent as i32 - 15)
                    }
                };
                value.to_bits()
            }
            _ => unreachable!(),
        };
    }
    (result, floating)
}

#[cfg(windows)]
fn packed_cases(format: u8) -> Vec<u32> {
    if !matches!(format, 36 | 43) {
        return vec![0, u32::MAX, 0x89ab_cdef, 0x0123_4567];
    }
    let widths = if format == 36 {
        [11, 11, 10]
    } else {
        [10, 11, 11]
    };
    let mut words = vec![0x48f6_7a03];
    for case in 0..8 {
        let mut offset = 0;
        let mut word = 0;
        for width in widths {
            let mantissa = width - 5;
            let low = (1 << mantissa) - 1;
            let field = match case {
                0 => 0,
                1 => 1,
                2 => low,
                3 => 1 << mantissa,
                4 => 15 << mantissa,
                5 => (30 << mantissa) | low,
                6 => 31 << mantissa,
                _ => (31 << mantissa) | 1,
            };
            word |= field << offset;
            offset += width;
        }
        words.push(word);
    }
    words
}

#[test]
#[cfg(windows)]
fn packed_buffer_formats_read_expected_channels() {
    use super::host::{Dispatch, Host, Root, Stride, View};
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut validator =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
            .expect("packed format test requires dxil.dll");
    let shaders: Vec<_> = (30..=49)
        .map(|format| {
            let shader = build(Op::Packed(format), 0, 0).unwrap();
            validator.validate(&mut shader.dxil.clone()).unwrap();
            (format, shader)
        })
        .collect();
    let structured: Vec<_> = [36, 43, 48, 64, 74, 77]
        .into_iter()
        .flat_map(|format| [4, 8, 12].map(move |stride| (format, stride)))
        .map(|(format, stride)| {
            let shader = build(Op::Packed(format), stride, 0).unwrap();
            validator.validate(&mut shader.dxil.clone()).unwrap();
            (format, stride, shader)
        })
        .collect();
    let (mut backends, mut cases) = (0, 0);
    let mut structured_cases = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            continue;
        };
        for (format, shader) in &shaders {
            let (root, pso) = host.compute_pipeline(shader).unwrap();
            for word in packed_cases(*format) {
                let mut ud = vec![0; shader.user_data_dwords() as usize];
                for u in &shader.user_data {
                    ud[u.index as usize] = match (u.root, u.slot) {
                        (RootType::Constants, 0) => u32::from(u.dword == 3),
                        (RootType::Table, 1) => 0,
                        (RootType::Table, 2) => 32,
                        x => panic!("unexpected user data {x:?}"),
                    };
                }
                let data = [word];
                let views = [
                    View::Buffer {
                        data: &data,
                        stride: Stride::Raw,
                    },
                    View::RawUav(5),
                ];
                let out = host
                    .dispatch(
                        &root,
                        &pso,
                        &Dispatch {
                            roots: vec![(0, Root::Constants(ud))],
                            views: &views,
                            samplers: &[],
                            groups: [1, 1, 1],
                        },
                    )
                    .unwrap();
                let (expected, floating) = packed_channels(*format, word);
                assert_eq!(out[0][4], 4, "raw descriptor byte count");
                for (channel, (&got, want)) in out[0][..4].iter().zip(expected).enumerate() {
                    let equal = if floating && f32::from_bits(want).is_nan() {
                        f32::from_bits(got).is_nan()
                    } else if floating && !matches!(format, 36 | 43) {
                        // Existing normalized division can be one ULP from
                        // correctly rounded conversion on AMD adapters.
                        got.abs_diff(want) <= 1
                    } else {
                        got == want
                    };
                    assert!(
                        equal,
                        "{label}: format{format}, word{word:#x}, channel{channel}: {got:#x} != {want:#x}"
                    );
                }
                cases += 1;
            }
        }
        for (format, stride, shader) in &structured {
            let (root, pso) = host.compute_pipeline(shader).unwrap();
            let wide = *format >= 64;
            let data: Vec<_> = (0..WORDS)
                .map(|n| {
                    if wide {
                        (n as f32 * 0.5 - 4.0).to_bits()
                    } else {
                        0x89ab_cdef ^ (n * 0x0102_0304)
                    }
                })
                .collect();
            let offsets: &[(u32, u32)] = if wide {
                &[(0, 0), (stride - 4, 4), (4 * WORDS - 4, 0)]
            } else {
                &[(0, 0)]
            };
            for &(member, scalar) in offsets {
                let inputs = [0, member, scalar, 1, 0, 0, 0, 0];
                let mut ud = vec![0; shader.user_data_dwords() as usize];
                for u in &shader.user_data {
                    ud[u.index as usize] = match (u.root, u.slot) {
                        (RootType::Constants, 0) => inputs[u.dword as usize],
                        (RootType::Table, 1) => 0,
                        (RootType::Table, 2) => 32,
                        x => panic!("unexpected user data {x:?}"),
                    };
                }
                let views = [
                    View::Buffer {
                        data: &data,
                        stride: Stride::Structured(*stride),
                    },
                    View::RawUav(5),
                ];
                let out = host
                    .dispatch(
                        &root,
                        &pso,
                        &Dispatch {
                            roots: vec![(0, Root::Constants(ud))],
                            views: &views,
                            samplers: &[],
                            groups: [1, 1, 1],
                        },
                    )
                    .unwrap();
                assert_eq!(
                    out[0][4],
                    4 * WORDS / stride,
                    "structured descriptor record count"
                );
                let expected = if wide {
                    let n = match format {
                        64 => 2,
                        74 => 3,
                        77 => 4,
                        _ => unreachable!(),
                    };
                    let mut result = [0, 0, 0, if n < 4 { 1.0f32.to_bits() } else { 0 }];
                    let at = (member + scalar) / 4;
                    if at + n <= WORDS {
                        result[..n as usize].copy_from_slice(&data[at as usize..(at + n) as usize]);
                    }
                    result
                } else {
                    packed_channels(*format, data[0]).0
                };
                for (&got, want) in out[0][..4].iter().zip(expected) {
                    let equal = if matches!(format, 36 | 43) && f32::from_bits(want).is_nan() {
                        f32::from_bits(got).is_nan()
                    } else {
                        got == want
                    };
                    assert!(
                        equal,
                        "{label}: structured format{format} stride{stride} at{member}+{scalar}: {out:?} != {expected:?}"
                    );
                }
                cases += 1;
                structured_cases += 1;
            }
        }
        backends += 1;
    }
    assert!(
        backends > 0,
        "packed buffer fixture requires a native SM6.6 backend"
    );
    assert_eq!(cases, 126 * backends);
    assert_eq!(structured_cases, 36 * backends);
    eprintln!(
        "VICTORY_PACKED_BUFFER_RESULT=executed formats=23 backends={backends} cases={cases} structured_cases={structured_cases}"
    );
}

#[cfg(windows)]
fn expected(op: Op, stride: u32, immediate: u16, inputs: [u32; 8]) -> (Vec<u32>, Vec<u32>) {
    let [index, member, scalar, active, a, b, c, d] = inputs;
    let values = [a, b, c, d];
    let mut result = values.to_vec();
    let mut data: Vec<u32> = (0..WORDS).map(|n| 1000 + n).collect();
    let mut linear = values;
    if active != 0 {
        for (k, value) in linear.iter_mut().enumerate() {
            let at = u64::from(member) + u64::from(immediate) + u64::from(scalar) + 4 * k as u64;
            *value = if at < u64::from(4 * WORDS) {
                data[(at / 4) as usize]
            } else {
                0
            };
        }
    }
    let n = match op {
        Op::Load(n) | Op::Store(n) => n,
        Op::Atomic(..) => 1,
        Op::Packed(_) => unreachable!(),
    };
    if active != 0 {
        for k in 0..n {
            // Independent host oracle uses full-width arithmetic; a record
            // check and an exact raw-window check are separate constraints.
            let member = u64::from(member) + u64::from(immediate) + 4 * u64::from(k);
            let address = u64::from(index) * u64::from(stride) + member + u64::from(scalar);
            let valid = index < 4 * WORDS / stride
                && member < u64::from(stride)
                && address < u64::from(4 * WORDS);
            let word = (address / 4) as usize;
            match op {
                Op::Load(_) => result[usize::from(k)] = if valid { data[word] } else { 0 },
                Op::Store(_) if valid => data[word] = values[usize::from(k)],
                Op::Store(_) => {}
                Op::Atomic(code, returns) => {
                    let old = if valid { data[word] } else { 0 };
                    if valid {
                        data[word] = match code {
                            0x32 => old.wrapping_add(a),
                            0x31 => {
                                if old == b {
                                    a
                                } else {
                                    old
                                }
                            }
                            _ => unreachable!(),
                        };
                    }
                    if returns {
                        result[0] = old;
                    }
                }
                Op::Packed(_) => unreachable!(),
            }
        }
    }
    result.extend(linear);
    result.push(4 * WORDS / stride);
    if matches!(op, Op::Atomic(..)) {
        result.push(if active != 0 { linear[0] } else { 0 });
    }
    (data, result)
}

#[test]
#[cfg(windows)]
fn mixed_buffer_accesses_preserve_structured_views() {
    use super::host::{Dispatch, Host, Root, Stride, View};
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut validator =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
            .expect("buffer test requires dxil.dll");
    let source: Vec<u32> = (0..WORDS).map(|n| 1000 + n).collect();
    let (mut programs, mut executions, mut cases) = (0, 0, 0);
    for stride in STRIDES {
        for (op, immediate) in [
            (Op::Load(4), 0),
            (Op::Load(4), 4),
            (Op::Store(4), 0),
            (Op::Atomic(0x32, true), 0),
            (Op::Atomic(0x31, true), 0),
        ] {
            let shader = build(op, stride, immediate).unwrap();
            validator
                .validate(&mut shader.dxil.clone())
                .expect("indexed buffer DXIL validation");
            let records = 4 * WORDS / stride;
            let addresses = [
                [0, 0, 0, 1],
                [records - 1, 0, 0, 1],
                [1, 4, 0, 1],
                [0, stride - 4, 0, 1],
                [0, stride, 0, 1],
                [records, 0, 0, 1],
                [u32::MAX, 0, 0, 1],
                [0x4000_0000, 0, 0, 1],
                [0, 0, stride, 1],
                [0, 0, 4 * WORDS, 1],
                [0, u32::MAX - 3, 8, 1],
                [1, 0, 0, 0],
                [0, 3, 0, 1],
            ];
            let views = [
                if op.uav() {
                    View::StructuredUav(WORDS, stride)
                } else {
                    View::Buffer {
                        data: &source,
                        stride: Stride::Structured(stride),
                    }
                },
                View::RawUav(if matches!(op, Op::Atomic(..)) { 10 } else { 9 }),
            ];
            let mut ran = 0;
            for (label, warp) in [("hardware", false), ("warp", true)] {
                let Ok(host) = Host::new(warp) else {
                    continue;
                };
                let (root, pso) = host.compute_pipeline(&shader).unwrap();
                for address in addresses {
                    let mut inputs = [0; 8];
                    inputs[..4].copy_from_slice(&address);
                    inputs[4..].copy_from_slice(&[0x1234_5678, 1000, 0x9abc_def0, 0x5555_aaaa]);
                    let mut ud = vec![0; shader.user_data_dwords() as usize];
                    for u in &shader.user_data {
                        ud[u.index as usize] = match (u.root, u.slot) {
                            (RootType::Constants, 0) => inputs[u.dword as usize],
                            (RootType::Table, 1) => 0,
                            (RootType::Table, 2) => 32,
                            x => panic!("unexpected user data {x:?}"),
                        };
                    }
                    let out = host
                        .dispatch(
                            &root,
                            &pso,
                            &Dispatch {
                                roots: vec![(0, Root::Constants(ud))],
                                views: &views,
                                samplers: &[],
                                groups: [1, 1, 1],
                            },
                        )
                        .unwrap();
                    let (memory, result) = expected(op, stride, immediate, inputs);
                    assert_eq!(
                        out.last().unwrap(),
                        &result,
                        "{label}: {op:?}, stride{stride}, immediate{immediate}, {address:?}"
                    );
                    if op.uav() {
                        assert_eq!(
                            out[0], memory,
                            "{label}: memory {op:?}, stride{stride}, immediate{immediate}, {address:?}"
                        );
                    }
                    cases += 1;
                }
                ran += 1;
            }
            assert!(
                ran > 0,
                "indexed buffer fixture requires a native SM6.6 backend"
            );
            programs += 1;
            executions += ran;
        }
    }
    eprintln!(
        "VICTORY_INDEXED_BUFFER_RESULT=executed programs={programs} executions={executions} cases={cases}"
    );
}
