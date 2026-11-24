//! Synthetic image atomics. No title bytes or console compiler are needed.
//! Compilation tests run everywhere; execution tests require a Windows
//! SM 6.6 adapter with typed and descriptor-heap 64-bit atomic support.
//! Set `VICTORY_DXV` to the official DXC `dxv` executable to validate all
//! 88 generated containers on any platform in addition to compilation.
//!
//! Typed 64-bit UAV format and capability requirements:
//! <https://microsoft.github.io/DirectX-Specs/d3d/HLSL_SM_6_6_Int64_and_Float_Atomics.html>.

use super::{HostShader, lower};
use crate::dxbc::{Container, FourCC};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const OPS: &[u8] = &[
    0x0f, 0x10, 0x11, 0x12, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a,
];

const fn s_mov(dst: u8, src: u8) -> u32 {
    0xBE80_0300 | (dst as u32) << 16 | src as u32
}

const fn v_mov(dst: u8, src: u16) -> u32 {
    0x7E00_0200 | (dst as u32) << 17 | src as u32
}

/// Synthetic MIMG, a 2D integer image at s[16:23], coordinates v[8:9].
fn atomic(op: u8, data: u8, dmask: u8, glc: bool) -> [u32; 2] {
    [
        0xF000_0008 | u32::from(op) << 18 | u32::from(dmask) << 8 | u32::from(glc) << 13,
        4 << 16 | u32::from(data) << 8 | 8,
    ]
}

fn store(data: u8, offset: u16) -> [u32; 2] {
    [
        0xE070_0000 | u32::from(offset),
        0x8003_0000 | u32::from(data) << 8,
    ]
}

/// Root constants: initial u64, value u64, comparison u64, active mask,
/// unused. Two tables bind the raw result UAV and the integer image UAV.
pub(super) fn program(
    op: u8,
    wide: bool,
    glc: bool,
    guarded: bool,
    mask: Option<u8>,
) -> NativeProgram {
    let mut words = vec![s_mov(9, 0x80), s_mov(11, 0x80)];
    // s_load_dwordx4 s[12:15], s[8:9], 0
    words.extend([0xF408_0304, 125 << 25]);
    // s_load_dwordx8 s[16:23], s[10:11], 0
    words.extend([0xF40C_0405, 125 << 25]);
    words.extend([v_mov(8, 0x80), v_mov(9, 0x80)]);
    words.extend([v_mov(1, 0), v_mov(2, 1)]);
    let single_mask = if wide { 3 } else { 1 };
    words.extend(atomic(0x0f, 1, single_mask, false)); // initialize
    words.extend([v_mov(3, 2), v_mov(4, if wide { 3 } else { 4 })]);
    words.extend([v_mov(5, 4), v_mov(6, 5)]);
    if guarded {
        words.push(s_mov(126, 6)); // exec_lo = root constant active mask
    }
    let dmask = mask.unwrap_or(if op == 0x10 {
        single_mask | single_mask << if wide { 2 } else { 1 }
    } else {
        single_mask
    });
    words.extend(atomic(op, 3, dmask, glc));
    if guarded {
        words.push(s_mov(126, 0x81)); // restore the only thread
    }
    words.extend(store(3, 0));
    if wide {
        words.extend(store(4, 4));
    }
    // Swap zero into the image to retrieve the final value as raw words.
    words.extend([v_mov(1, 0x80), v_mov(2, 0x80)]);
    words.extend(atomic(0x0f, 1, single_mask, true));
    words.extend(store(1, 8));
    if wide {
        words.extend(store(2, 12));
    }
    words.push(0xBF81_0000);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
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
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 10,
                slot: 2,
            },
        ],
        used: vec![
            UsedResource {
                kind: ResourceType::Uav,
                root: RootType::Table,
                table_offset_dwords: 0,
                lower_bound: 0,
                stride: 0,
                dwords: 4,
                user_data: 8,
                slot: 1,
            },
            UsedResource {
                kind: ResourceType::Uav,
                root: RootType::Table,
                table_offset_dwords: 0,
                lower_bound: 0,
                stride: 0,
                dwords: 8,
                user_data: 10,
                slot: 2,
            },
        ],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 11 << 1,
            threads: [1, 1, 1],
        }),
        code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
    }
}

fn build(
    op: u8,
    wide: bool,
    glc: bool,
    guarded: bool,
    mask: Option<u8>,
) -> Result<HostShader, ShaderError> {
    let native = program(op, wide, glc, guarded, mask);
    let p = ir::build(&native.code, native.wave64, native.vgprs)?;
    let res = resource::track(&p, &native.layout, native.stage)?;
    let io = io::raise(&p, &native)?;
    let st = structure::structure(&p)?;
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
fn image_atomics_translate_in_both_widths_and_return_modes() {
    let dxv = std::env::var_os("VICTORY_DXV");
    #[cfg(windows)]
    let mut validator =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
            .ok();
    for &op in OPS {
        for wide in [false, true] {
            for glc in [false, true] {
                for guarded in [false, true] {
                    let shader = build(op, wide, glc, guarded, None).unwrap_or_else(|e| {
                        panic!("op={op:#x}, wide={wide}, glc={glc}, guarded={guarded}: {e}")
                    });
                    assert_eq!(shader.requires_i64_image_atomics(), wide);
                    let c = Container::parse(&shader.dxil).unwrap();
                    let features =
                        u64::from_le_bytes(c.part(FourCC::SFI0).unwrap().data.try_into().unwrap());
                    assert_eq!(
                        features & (0x40_0000 | 0x1000_0000 | 0x8000),
                        if wide { 0x1040_8000 } else { 0 }
                    );
                    assert_eq!(features & 0x4000 != 0, guarded);
                    if let Some(exe) = &dxv {
                        validate_dxv(exe, &shader.dxil);
                    }
                    #[cfg(windows)]
                    if let Some(validator) = &mut validator {
                        validator
                            .validate(&mut shader.dxil.clone())
                            .unwrap_or_else(|e| {
                                panic!("op={op:#x}, wide={wide}, glc={glc}, guarded={guarded}: {e}")
                            });
                    }
                }
            }
        }
    }
}

fn validate_dxv(exe: &std::ffi::OsStr, dxil: &[u8]) {
    use std::io::Write;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "victory-atomic-{}-{stamp}.dxil",
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(dxil).unwrap();
    drop(file);
    let result = std::process::Command::new(exe).arg(&path).output();
    std::fs::remove_file(&path).unwrap();
    let output = result.expect("could not run VICTORY_DXV");
    assert!(
        output.status.success(),
        "DXIL validator: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn image_atomic_invalid_data_masks_are_rejected() {
    for (op, masks) in [(0x11, &[2, 5, 7, 15][..]), (0x10, &[1, 2, 7, 12][..])] {
        for &mask in masks {
            let e = build(op, true, true, false, Some(mask)).unwrap_err();
            assert!(e.to_string().contains("image atomic data mask"), "{e}");
        }
    }
}

#[test]
fn atomic_requirements_come_from_the_container() {
    let mut s = build(0x11, true, true, false, None).unwrap();
    let restored = s.clone();
    assert!(restored.requires_i64_image_atomics());
    s.dxil = build(0x11, false, true, false, None).unwrap().dxil;
    assert!(!s.requires_i64_image_atomics());
}

/// High halves differ and add/sub cross the 32-bit boundary. Compare/swap
/// exercises success and failure; min/max include opposite signed halves.
#[cfg(windows)]
#[test]
fn image_atomic64_results_match_integer_semantics() {
    use super::host::{Dispatch, Host, Root, View};
    use windows::Win32::Graphics::Direct3D12::*;

    let mut ran = 0;
    let adapters: &[(&str, bool)] = if std::env::var("VICTORY_WARP").is_ok_and(|v| v == "1") {
        &[("warp", true)]
    } else {
        &[("default adapter", false), ("warp", true)]
    };
    for &(label, warp) in adapters {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let mut opt1 = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
        let mut opt9 = D3D12_FEATURE_DATA_D3D12_OPTIONS9::default();
        let mut opt11 = D3D12_FEATURE_DATA_D3D12_OPTIONS11::default();
        // SAFETY: each pointer names the initialized structure matching
        // the feature enum and lives through the synchronous query.
        let supported = unsafe {
            host.device
                .CheckFeatureSupport(
                    D3D12_FEATURE_D3D12_OPTIONS1,
                    (&raw mut opt1).cast(),
                    size_of_val(&opt1) as u32,
                )
                .is_ok()
                && host
                    .device
                    .CheckFeatureSupport(
                        D3D12_FEATURE_D3D12_OPTIONS9,
                        (&raw mut opt9).cast(),
                        size_of_val(&opt9) as u32,
                    )
                    .is_ok()
                && host
                    .device
                    .CheckFeatureSupport(
                        D3D12_FEATURE_D3D12_OPTIONS11,
                        (&raw mut opt11).cast(),
                        size_of_val(&opt11) as u32,
                    )
                    .is_ok()
        };
        if !supported
            || !opt1.Int64ShaderOps.as_bool()
            || !opt9.AtomicInt64OnTypedResourceSupported.as_bool()
            || !opt11.AtomicInt64OnDescriptorHeapResourceSupported.as_bool()
        {
            eprintln!("{label} lacks typed/heap 64-bit atomics; skipped");
            continue;
        }
        ran += 1;
        let cases: &[(u64, u64)] = &[
            (0x0000_0001_ffff_ffff, 1),
            (0x1234_5678_0000_0000, 0x0000_0010_ffff_ffff),
            (0xffff_ffff_ffff_fff0, 0x0000_0001_0000_0020),
            (0x8000_0000_0000_0000, 0x7fff_ffff_ffff_ffff),
        ];
        for &op in OPS {
            for glc in [false, true] {
                for guarded in [false, true] {
                    let shader = build(op, true, glc, guarded, None).unwrap();
                    let (root, pso) = host
                        .compute_pipeline(&shader)
                        .unwrap_or_else(|e| panic!("{label}: {e}"));
                    for &(initial, value) in cases {
                        for matches in [false, true] {
                            for active in [false, true] {
                                let comparison = if matches {
                                    initial
                                } else {
                                    initial ^ (1 << 40)
                                };
                                let inputs = [
                                    initial as u32,
                                    (initial >> 32) as u32,
                                    value as u32,
                                    (value >> 32) as u32,
                                    comparison as u32,
                                    (comparison >> 32) as u32,
                                    u32::from(active),
                                    0,
                                ];
                                let mut data = vec![0; shader.user_data_dwords() as usize];
                                for u in &shader.user_data {
                                    data[u.index as usize] = match (u.root, u.slot) {
                                        (RootType::Constants, 0) => inputs[u.dword as usize],
                                        (RootType::Table, 1) => 0,
                                        (RootType::Table, 2) => 32,
                                        other => panic!("unexpected user data {other:?}"),
                                    };
                                }
                                let out = host
                                    .dispatch(
                                        &root,
                                        &pso,
                                        &Dispatch {
                                            roots: vec![(0, Root::Constants(data))],
                                            views: &[View::RawUav(4), View::Uint64TextureUav(1, 1)],
                                            samplers: &[],
                                            groups: [1, 1, 1],
                                        },
                                    )
                                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                                let actual_returned =
                                    u64::from(out[0][0]) | u64::from(out[0][1]) << 32;
                                let actual_final =
                                    u64::from(out[0][2]) | u64::from(out[0][3]) << 32;
                                let expected = if guarded && !active {
                                    initial
                                } else {
                                    match op {
                                        0x0f => value,
                                        0x10 => {
                                            if matches {
                                                value
                                            } else {
                                                initial
                                            }
                                        }
                                        0x11 => initial.wrapping_add(value),
                                        0x12 => initial.wrapping_sub(value),
                                        0x14 => (initial as i64).min(value as i64) as u64,
                                        0x15 => initial.min(value),
                                        0x16 => (initial as i64).max(value as i64) as u64,
                                        0x17 => initial.max(value),
                                        0x18 => initial & value,
                                        0x19 => initial | value,
                                        0x1a => initial ^ value,
                                        _ => unreachable!(),
                                    }
                                };
                                let returned = if glc && (!guarded || active) {
                                    initial
                                } else {
                                    value
                                };
                                assert_eq!(
                                    (actual_returned, actual_final),
                                    (returned, expected),
                                    "{label}, op={op:#x}, initial={initial:#x}, value={value:#x}, matches={matches}, glc={glc}, guarded={guarded}, active={active}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    if ran == 0 {
        eprintln!("64-bit image atomics execution NOT validated: no capable adapter");
    }
}
