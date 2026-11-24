//! Authored LDS byte/halfword programs, including adjacent-byte contention.
//! AMD RDNA2 ISA sections 10.4.3 and 12.13 define these operations:
//! <https://docs.amd.com/v/u/en-US/rdna2-shader-instruction-set-architecture>.
//! No title data or console compiler is used.

use super::{HostShader, lower};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const THREADS: u32 = 128;
const LDS_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Separate,
    #[cfg_attr(not(windows), allow(dead_code))]
    Adjacent,
    /// Consecutive host dwords receive fields from threads 32/64 apart.
    /// Own-field reads run before the barrier while other waves may write.
    Striped,
}

#[derive(Clone, Copy, Debug)]
struct Read {
    op: u8,
    bits: u32,
    #[cfg_attr(not(windows), allow(dead_code))]
    signed: bool,
    /// None: whole dword; Some(false/true): low/high destination half.
    #[cfg_attr(not(windows), allow(dead_code))]
    half: Option<bool>,
}

const READS: &[Read] = &[
    Read {
        op: 0x39,
        bits: 8,
        signed: true,
        half: None,
    },
    Read {
        op: 0x3a,
        bits: 8,
        signed: false,
        half: None,
    },
    Read {
        op: 0x3b,
        bits: 16,
        signed: true,
        half: None,
    },
    Read {
        op: 0x3c,
        bits: 16,
        signed: false,
        half: None,
    },
    Read {
        op: 0xa2,
        bits: 8,
        signed: false,
        half: Some(false),
    },
    Read {
        op: 0xa3,
        bits: 8,
        signed: false,
        half: Some(true),
    },
    Read {
        op: 0xa4,
        bits: 8,
        signed: true,
        half: Some(false),
    },
    Read {
        op: 0xa5,
        bits: 8,
        signed: true,
        half: Some(true),
    },
    Read {
        op: 0xa6,
        bits: 16,
        signed: false,
        half: Some(false),
    },
    Read {
        op: 0xa7,
        bits: 16,
        signed: false,
        half: Some(true),
    },
];

const fn v_mov(dst: u8, src: u16) -> u32 {
    0x7e00_0200 | (dst as u32) << 17 | src as u32
}

const fn vop2(op: u8, dst: u8, src0: u16, vsrc1: u8) -> u32 {
    (op as u32) << 25 | (dst as u32) << 17 | (vsrc1 as u32) << 9 | src0 as u32
}

const fn ds(op: u8, addr: u8, data: u8, dst: u8, offset: u16) -> [u32; 2] {
    [
        0xd800_0000 | (op as u32) << 18 | offset as u32,
        (dst as u32) << 24 | (data as u32) << 8 | addr as u32,
    ]
}

const fn store(data: u8, offset: u16) -> [u32; 2] {
    // buffer_store_dword vdata, v7, s[8:11], 0 offen
    [
        0xe070_1000 | offset as u32,
        0x8002_0007 | (data as u32) << 8,
    ]
}

/// Four constants: initial shared dword, store source, destination sentinel,
/// address bias. All threads first initialize different LDS dwords and meet
/// at a barrier. The subword addresses either occupy separate dwords or
/// pack neighboring or widely separated threads into the same host dword.
fn build(read: Read, high: bool, offset: u16, layout: Layout, wave64: bool) -> HostShader {
    let write = match (read.bits, high) {
        (8, false) => 0x1e,
        (8, true) => 0xa0,
        (16, false) => 0x1f,
        (16, true) => 0xa1,
        _ => unreachable!(),
    };
    let mut words = vec![
        0xbe85_0380, // s_mov_b32 s5, 0: descriptor-table high half
        0xf408_0202,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
        v_mov(1, 0),
        v_mov(2, 1),
        v_mov(3, 2),
        vop2(0x1a, 4, 0x82, 0), // v4 = thread * 4
    ];
    words.extend(ds(0x0d, 4, 1, 0, 0)); // ds_write_b32
    words.push(0xbf8a_0000); // s_barrier
    match layout {
        Layout::Separate => words.push(v_mov(6, 260)), // v6 = v4
        Layout::Adjacent if read.bits == 8 => words.push(v_mov(6, 256)), // thread
        Layout::Adjacent => words.push(vop2(0x1a, 6, 0x81, 0)), // thread * 2
        Layout::Striped => {
            let (mask, shift) = if read.bits == 8 { (31, 5) } else { (63, 6) };
            words.push(vop2(0x1b, 6, 0x80 + mask, 0)); // thread % fields
            words.push(vop2(0x1a, 6, 0x82, 6)); // dword byte address
            words.push(vop2(0x16, 5, 0x80 + shift, 0)); // thread / fields
            if read.bits == 16 {
                words.push(vop2(0x1a, 5, 0x81, 5));
            }
            words.push(vop2(0x1c, 6, 261, 6)); // add field's byte position
        }
    }
    if layout != Layout::Separate {
        // Each writer contributes a different field, including high-half
        // source selection; preserving neighbors is observable afterwards.
        words.push(vop2(0x1a, 5, if high { 0x90 } else { 0x80 }, 0));
        words.push(vop2(0x1c, 2, 261, 2)); // v2 |= v5
    }
    words.push(vop2(0x25, 6, 3, 6)); // address += s3
    words.extend(ds(write, 6, 2, 0, offset));
    if layout != Layout::Striped {
        words.push(0xbf8a_0000); // All writes, including other waves, finish.
    }
    words.extend(ds(read.op, 6, 0, 3, offset));
    if layout == Layout::Striped {
        words.push(0xbf8a_0000); // Whole-word observation needs every writer.
    }
    words.extend(ds(0x36, 4, 0, 5, 0)); // Whole dword detects clobbered neighbors.
    words.push(vop2(0x1a, 7, 0x83, 0)); // Each thread owns two output dwords.
    words.extend(store(3, 0));
    words.extend(store(5, 4));
    words.push(0xbf81_0000);
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 8,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: (5 << 1) | ((LDS_BYTES as u32 / 512) << 15),
            threads: [THREADS, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).unwrap();
    let res = resource::track(&p, &native.layout, native.stage).unwrap();
    let io = io::raise(&p, &native).unwrap();
    let st = structure::structure(&p).unwrap();
    let out = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .unwrap();
    HostShader {
        stage: HwStage::Cs,
        dxil: out.dxil,
        user_data: out.user_data,
        samplers: out.samplers,
        static_samplers: Vec::new(),
        root_cbvs: out.root_cbvs,
        root_srvs: out.root_srvs,
    }
}

fn validate_dxv(executable: &std::ffi::OsStr, shader: &HostShader) {
    use std::io::Write;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("victory-lds-{}-{stamp}.dxil", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(&shader.dxil).unwrap();
    drop(file);
    let result = std::process::Command::new(executable).arg(&path).output();
    std::fs::remove_file(&path).unwrap();
    let result = result.expect("could not run VICTORY_DXV");
    assert!(
        result.status.success(),
        "DXIL validation: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn lds_subword_programs_translate_and_validate() {
    let dxv = std::env::var_os("VICTORY_DXV");
    #[cfg(windows)]
    let mut validator = match super::validator::Validator::load(
        super::validator::Validator::default_path().as_deref(),
    ) {
        Ok(v) => Some(v),
        Err(e) if std::env::var_os("VICTORY_DXIL").is_some() => {
            panic!("requested DXIL validator unavailable: {e}")
        }
        Err(e) => {
            eprintln!("DXIL validator unavailable: {e}");
            None
        }
    };
    let mut count = 0;
    for &read in READS {
        for high in [false, true] {
            let variants = (0..4)
                .step_by(read.bits as usize / 8)
                .map(|o| (o, Layout::Separate))
                .chain([(0, Layout::Striped)]);
            for (offset, layout) in variants {
                for wave64 in [false, true] {
                    let shader = build(read, high, offset, layout, wave64);
                    assert_eq!(shader.required_wave_size(), None);
                    if let Some(dxv) = &dxv {
                        validate_dxv(dxv, &shader);
                    }
                    #[cfg(windows)]
                    if let Some(v) = &mut validator {
                        v.validate(&mut shader.dxil.clone()).unwrap();
                    }
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 168);
    #[cfg(windows)]
    if validator.is_some() {
        eprintln!("VICTORY_LDS_VALIDATION_RESULT=validated containers={count}");
    }
}

#[cfg(windows)]
fn expected(read: Read, high: bool, offset: u16, layout: Layout, inputs: [u32; 4]) -> Vec<u32> {
    let mut memory = vec![0u8; LDS_BYTES];
    for word in memory.as_chunks_mut::<4>().0 {
        word.copy_from_slice(&inputs[0].to_le_bytes());
    }
    let bytes = read.bits / 8;
    let address = |thread: u32| {
        let base = match layout {
            Layout::Separate => thread * 4,
            Layout::Adjacent => thread * bytes,
            Layout::Striped => {
                let fields = THREADS / (4 / bytes);
                thread % fields * 4 + thread / fields * bytes
            }
        };
        base.wrapping_add(inputs[3])
            .checked_add(u32::from(offset))
            .map(|n| n & !(bytes - 1))
            .filter(|&n| n <= LDS_BYTES as u32 - bytes)
            .map(|n| n as usize)
    };
    for thread in 0..THREADS {
        if let Some(address) = address(thread) {
            let source = inputs[1]
                | if layout != Layout::Separate {
                    thread << if high { 16 } else { 0 }
                } else {
                    0
                };
            let source = if high { source >> 16 } else { source };
            memory[address..address + bytes as usize]
                .copy_from_slice(&source.to_le_bytes()[..bytes as usize]);
        }
    }
    let mut out = Vec::new();
    for thread in 0..THREADS {
        let value = match address(thread) {
            Some(a) if read.bits == 8 => {
                if read.signed {
                    (memory[a] as i8 as i32) as u32
                } else {
                    u32::from(memory[a])
                }
            }
            Some(a) => {
                let value = u16::from_le_bytes([memory[a], memory[a + 1]]);
                if read.signed {
                    (value as i16 as i32) as u32
                } else {
                    u32::from(value)
                }
            }
            None => 0,
        };
        out.push(match read.half {
            None => value,
            Some(false) => inputs[2] & 0xffff_0000 | value & 0xffff,
            Some(true) => inputs[2] & 0xffff | value << 16,
        });
        let a = thread as usize * 4;
        out.push(u32::from_le_bytes(memory[a..a + 4].try_into().unwrap()));
    }
    out
}

#[cfg(windows)]
#[test]
fn lds_subword_results_preserve_adjacent_bytes() {
    use super::host::{Dispatch, Host, Root, View};
    let required = std::env::var("VICTORY_WARP").is_ok_and(|v| v == "1");
    let adapters: &[(&str, bool)] = if required {
        &[("warp", true)]
    } else {
        &[("default adapter", false), ("warp", true)]
    };
    let mut ran = 0;
    let mut count = 0;
    for &(label, warp) in adapters {
        let host = match Host::new(warp) {
            Ok(host) => host,
            Err(e) if required => panic!("required WARP device unavailable: {e}"),
            Err(e) => {
                eprintln!("{label} unavailable: {e}");
                continue;
            }
        };
        ran += 1;
        for &read in READS {
            for high in [false, true] {
                // Separate fields, contended neighboring fields, immediate
                // offsets using both offset bytes, and past-the-end addresses.
                let variants = (0..4)
                    .step_by(read.bits as usize / 8)
                    .map(|o| (o, Layout::Separate))
                    .chain([
                        (0, Layout::Adjacent),
                        (0, Layout::Striped),
                        (256, Layout::Separate),
                    ]);
                for (offset, layout) in variants {
                    let shader = build(read, high, offset, layout, false);
                    let (root, pipeline) = host.compute_pipeline(&shader).unwrap();
                    for inputs in [
                        [0xa55a_c33c, 0x807f_80ff, 0x694b_1de7, 0],
                        [0x0000_0000, 0xffff_8000, 0xdead_beef, 0],
                        [0x7654_3210, 0x0080_ff7f, 0xa5c3_f00d, 512],
                        [0x89ab_cdef, 0x8000_7f80, 0xcafe_0123, 0xffff_ff00],
                    ] {
                        let mut user_data = vec![0; shader.user_data_dwords() as usize];
                        for u in &shader.user_data {
                            user_data[u.index as usize] = match (u.root, u.slot) {
                                (RootType::Constants, 0) => inputs[u.dword as usize],
                                (RootType::Table, 1) => 0,
                                other => panic!("unexpected user data {other:?}"),
                            };
                        }
                        let actual = host
                            .dispatch(
                                &root,
                                &pipeline,
                                &Dispatch {
                                    roots: vec![(0, Root::Constants(user_data))],
                                    views: &[View::RawUav(THREADS * 2)],
                                    samplers: &[],
                                    groups: [1, 1, 1],
                                },
                            )
                            .unwrap();
                        assert_eq!(
                            actual[0],
                            expected(read, high, offset, layout, inputs),
                            "{label}: {read:?}, high={high}, offset={offset}, layout={layout:?}, inputs={inputs:x?}"
                        );
                        count += 1;
                    }
                }
            }
        }
    }
    eprintln!(
        "VICTORY_LDS_RESULT={} adapters={ran} cases={count}",
        if ran > 0 { "executed" } else { "unsupported" }
    );
}
