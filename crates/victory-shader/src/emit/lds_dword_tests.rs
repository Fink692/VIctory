//! Authored single-dword LDS bounds/EXEC programs; no title data is used.
//! RDNA2 ISA 3.6.1, 3.6.6 and 10.4.3 describe allocation bounds and
//! ordinary indexed loads, stores and atomics:
//! <https://www.amd.com/content/dam/amd/en/documents/radeon-tech-docs/instruction-set-architectures/rdna2-shader-instruction-set-architecture.pdf>.
//! Multi-dword boundary, unaligned and ADD_TID modes are outside this test.

use super::{HostShader, lower};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const THREADS: u32 = 128;
const LDS_DWORDS: u32 = 128;
const OFFSETS: [u16; 2] = [0, 256];

#[derive(Clone, Copy, Debug)]
enum Access {
    Read,
    Write,
    Atomic { op: u8, returns: bool },
}

fn accesses() -> Vec<Access> {
    let mut out = vec![Access::Read, Access::Write];
    // add/sub, signed and unsigned min/max, and/or/xor, compare-exchange.
    for op in [0x00, 0x01, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x10] {
        for returns in [false, true] {
            out.push(Access::Atomic { op, returns });
        }
    }
    // Exchange has only a returning encoding.
    out.push(Access::Atomic {
        op: 0x0d,
        returns: true,
    });
    out
}

const fn v_mov(dst: u8, src: u16) -> u32 {
    0x7e00_0200 | (dst as u32) << 17 | src as u32
}

const fn vop2(op: u8, dst: u8, src0: u16, vsrc1: u8) -> u32 {
    (op as u32) << 25 | (dst as u32) << 17 | (vsrc1 as u32) << 9 | src0 as u32
}

const fn ds(op: u8, addr: u8, data0: u8, data1: u8, dst: u8, offset: u16) -> [u32; 2] {
    [
        0xd800_0000 | (op as u32) << 18 | offset as u32,
        (dst as u32) << 24 | (data1 as u32) << 16 | (data0 as u32) << 8 | addr as u32,
    ]
}

const fn store(data: u8, offset: u16) -> [u32; 2] {
    // buffer_store_dword vdata, v7, s[12:15], 0 offen
    [
        0xe070_1000 | offset as u32,
        0x8003_0007 | (data as u32) << 8,
    ]
}

/// Six root constants: initial seed, operand0 seed, operand1 seed,
/// destination sentinel, byte-address bias, and active-thread cutoff.
/// Each thread owns one initialized LDS dword. The tested access may
/// shift that address out of range; final readback observes every dword
/// to detect unintended writes. No two tested accesses alias each other.
fn build(access: Access, offset: u16, guarded: bool, wave64: bool) -> HostShader {
    let mut words = vec![
        0xbe89_0380, // s_mov_b32 s9, 0: descriptor-table high half
        0xf408_0304,
        125 << 25,              // s_load_dwordx4 s[12:15], s[8:9], 0
        vop2(0x25, 1, 0, 0),    // v1 = initial seed + thread
        vop2(0x25, 2, 1, 0),    // v2 = operand0 seed + thread
        vop2(0x25, 3, 2, 0),    // v3 = operand1 seed + thread
        v_mov(4, 3),            // v4 = destination sentinel
        vop2(0x1a, 5, 0x82, 0), // v5 = thread * 4
        vop2(0x25, 6, 4, 5),    // v6 = own byte address + bias
        vop2(0x1a, 7, 0x83, 0), // v7 = output offset: thread * 8
        v_mov(8, 5),            // v8 = active cutoff
    ];
    words.extend(ds(0x0d, 5, 1, 0, 0, 0)); // initialize own LDS dword
    words.push(0xbf8a_0000); // all initializing waves meet
    if guarded {
        // v_cmpx_lt_u32 vcc, v0, v8: only threads below the cutoff execute.
        words.push(0x7c00_0000 | 0x0d1 << 17 | 8 << 9 | 256);
    }
    let (op, d0, d1) = match access {
        Access::Read => (0x36, 0, 0),
        Access::Write => (0x0d, 2, 0),
        Access::Atomic { op, returns } => (op + if returns { 0x20 } else { 0 }, 2, 3),
    };
    words.extend(ds(op, 6, d0, d1, 4, offset));
    if guarded {
        words.push(if wave64 { 0xbefe_04c1 } else { 0xbefe_03c1 }); // restore EXEC
    }
    words.push(0xbf8a_0000); // all writes complete before the final snapshot
    words.extend(ds(0x36, 5, 0, 0, 9, 0));
    words.extend(store(4, 0));
    words.extend(store(9, 4));
    words.push(0xbf81_0000);
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 10,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 8,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 8,
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
            user_data: 8,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: (9 << 1) | ((LDS_DWORDS / 128) << 15),
            threads: [THREADS, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).unwrap();
    let res = resource::track(&p, &native.layout, native.stage, None).unwrap();
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
    .unwrap_or_else(|e| panic!("{access:?}, offset={offset}, guarded={guarded}: {e}"));
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
    let path = std::env::temp_dir().join(format!(
        "victory-lds-dword-{}-{stamp}.dxil",
        std::process::id()
    ));
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
fn lds_dword_programs_translate_and_validate() {
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
    for access in accesses() {
        for offset in OFFSETS {
            for guarded in [false, true] {
                for wave64 in [false, true] {
                    let shader = build(access, offset, guarded, wave64);
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
    assert_eq!(count, 184);
    #[cfg(windows)]
    if validator.is_some() {
        eprintln!("VICTORY_LDS_DWORD_VALIDATION_RESULT=validated containers={count}");
    }
}

#[cfg(windows)]
fn trace(args: std::fmt::Arguments<'_>) {
    if std::env::var("VICTORY_LDS_TRACE").is_ok_and(|v| v == "1") {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "VICTORY_LDS_DWORD_TRACE {args}").unwrap();
        stderr.flush().unwrap();
    }
}

#[cfg(windows)]
fn expected(access: Access, offset: u16, guarded: bool, inputs: [u32; 6]) -> Vec<u32> {
    let mut memory: Vec<u32> = (0..LDS_DWORDS).map(|t| inputs[0].wrapping_add(t)).collect();
    let mut results = vec![inputs[3]; THREADS as usize];
    for thread in 0..THREADS {
        if guarded && thread >= inputs[5] {
            continue;
        }
        let byte = (thread * 4)
            .wrapping_add(inputs[4])
            .wrapping_add(u32::from(offset));
        let index = (byte / 4) as usize;
        let old = memory.get(index).copied().unwrap_or(0);
        let data0 = inputs[1].wrapping_add(thread);
        let data1 = inputs[2].wrapping_add(thread);
        let value = match access {
            Access::Read => {
                results[thread as usize] = old;
                continue;
            }
            Access::Write => data0,
            Access::Atomic { op, returns } => {
                if returns {
                    results[thread as usize] = old;
                }
                match op {
                    0x00 => old.wrapping_add(data0),
                    0x01 => old.wrapping_sub(data0),
                    0x05 => (old as i32).min(data0 as i32) as u32,
                    0x06 => (old as i32).max(data0 as i32) as u32,
                    0x07 => old.min(data0),
                    0x08 => old.max(data0),
                    0x09 => old & data0,
                    0x0a => old | data0,
                    0x0b => old ^ data0,
                    0x0d => data0,
                    0x10 => {
                        if old == data0 {
                            data1
                        } else {
                            old
                        }
                    }
                    _ => unreachable!(),
                }
            }
        };
        if let Some(word) = memory.get_mut(index) {
            *word = value;
        }
    }
    results
        .into_iter()
        .zip(memory)
        .flat_map(|(r, m)| [r, m])
        .collect()
}

#[cfg(windows)]
#[test]
fn lds_dword_bounds_and_inactive_lanes_preserve_memory() {
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
        for access in accesses() {
            for offset in OFFSETS {
                for guarded in [false, true] {
                    let shader = build(access, offset, guarded, false);
                    trace(format_args!(
                        "phase=pipeline_begin adapter={label:?} access={access:?} offset={offset} guarded={guarded}"
                    ));
                    let (root, pipeline) = host.compute_pipeline(&shader).unwrap_or_else(|e| {
                        panic!("{label}: {access:?}, offset={offset}, guarded={guarded}: pipeline failed: {e}")
                    });
                    for inputs in [
                        [0xffff_fffc, 3, 0x1234_0000, 0xacce_55ed, 0, 128],
                        [0x7fff_fffc, 0x8000_0001, u32::MAX, 0x6bad_beef, 0, 64],
                        [0x8070_6050, 0x8070_6050, 0x0bad_0000, 0x1234_5678, 0, 96],
                        [0x1357_0000, 0x5555_0000, 0xaaaa_0000, 0xcafe_babe, 508, 128],
                        [0xdead_0000, 0x80, 0x44, 0x5555_aaaa, 512, 0],
                        [0x1020_3040, 0xffff, 0x30, 0x7865_4321, 0xffff_fc00, 64],
                    ] {
                        let mut user_data = vec![0; shader.user_data_dwords() as usize];
                        for u in &shader.user_data {
                            user_data[u.index as usize] = match (u.root, u.slot) {
                                (RootType::Constants, 0) => inputs[u.dword as usize],
                                (RootType::Table, 1) => 0,
                                other => panic!("unexpected user data {other:?}"),
                            };
                        }
                        trace(format_args!(
                            "phase=dispatch_begin adapter={label:?} case={} access={access:?} offset={offset} guarded={guarded} inputs={inputs:x?}",
                            count + 1
                        ));
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
                            .unwrap_or_else(|e| {
                                panic!(
                                    "{label}: case={}, {access:?}, offset={offset}, guarded={guarded}, inputs={inputs:x?}: dispatch failed: {e}",
                                    count + 1
                                )
                            });
                        assert_eq!(
                            actual[0],
                            expected(access, offset, guarded, inputs),
                            "{label}: {access:?}, offset={offset}, guarded={guarded}, inputs={inputs:x?}"
                        );
                        trace(format_args!(
                            "phase=dispatch_end adapter={label:?} case={}",
                            count + 1
                        ));
                        count += 1;
                    }
                }
            }
        }
    }
    eprintln!(
        "VICTORY_LDS_DWORD_RESULT={} adapters={ran} cases={count}",
        if ran > 0 { "executed" } else { "unsupported" }
    );
    assert_eq!(count, ran * 552);
    assert!(
        ran > 0,
        "neither a hardware nor WARP SM 6.6 device is available"
    );
}
