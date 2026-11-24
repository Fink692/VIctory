//! Authored texture-offset programs; no title shaders or assets.

use super::HostShader;
use super::container::SigElement;
use crate::native::{
    HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

fn offset_word(offsets: [i32; 3]) -> u32 {
    offsets.into_iter().enumerate().fold(0, |word, (axis, n)| {
        word | ((n as u32 & 0x3f) << (axis * 8))
    })
}

/// Sample the middle of a texture, with signed immediate texel offsets.
/// `None` uses a root constant instead, which DXIL cannot accept as an offset.
fn sample_offset(offset: Option<u32>, dim: u8) -> Result<HostShader, ShaderError> {
    let mut words = vec![
        0xBE84_0300, // s_mov_b32 s4, s0: resource table
        0xBE85_0380, // s_mov_b32 s5, 0
        0xBE86_0301, // s_mov_b32 s6, s1: sampler table
        0xBE87_0380, // s_mov_b32 s7, 0
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0
        0xF408_0203,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0
    ];
    match offset {
        Some(word) => words.extend([0x7E02_02FF, word]), // v_mov_b32 v1, literal
        None => words.push(0x7E02_0202),                 // v_mov_b32 v1, s2: dynamic offset
    }
    for reg in 2..=4 {
        words.extend([0x7E00_02FF | (reg << 17), 0.5f32.to_bits()]);
    }
    words.extend([
        0xF000_0000 | (0x30 << 18) | (15 << 8) | (u32::from(dim) << 3),
        0x0043_0C01, // image_sample_o v[12:15], v[1:4], s[12:19], s[8:11]
        0xF800_180F,
        0x0F0E_0D0C, // exp mrt0, v12, v13, v14, v15 done vm
        0xBF81_0000, // s_endpgm
    ]);
    let table = |user_data| RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot: user_data,
    };
    let range = |kind, user_data, dwords| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data,
        slot: user_data,
    };
    let native = NativeProgram {
        stage: HwStage::Ps,
        wave64: false,
        vgprs: 32,
        layout: vec![
            table(0),
            table(1),
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 2,
                slot: 2,
            },
        ],
        used: vec![
            range(ResourceType::Srv, 0, 8),
            range(ResourceType::Sampler, 1, 4),
        ],
        io: StageIo::Ps {
            inputs: Vec::new(),
            input_addr: 0,
            db_shader_control: 0,
            patches: Vec::new(),
        },
        code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
    };
    let output = SigElement {
        name: "SV_Target".into(),
        index: 0,
        kind: 16,
        comp: 9,
        interp: 0,
        row: Some(0),
        col: 0,
        cols: 4,
        usage: 15,
        half: false,
    };
    let signature = super::container::signature(&[output], false);
    let bytes = crate::dxbc::build(&[(crate::FourCC::OSG1, &signature)]);
    let container = crate::Container::parse(&bytes)?;
    let p = ir::build(&native.code, native.wave64, native.vgprs)?;
    let st = structure::structure(&p)?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
    let io = io::raise(&p, &native)?;
    super::emit(&container, &native, &p, &st, &io, &res)
}

#[test]
fn constant_sample_offsets_translate_only_in_the_host_range() {
    for dim in 0..=2 {
        for offsets in [[0, 0, 0], [-8, -8, -8], [7, 7, 7], [-1, 2, -3]] {
            sample_offset(Some(offset_word(offsets)), dim).unwrap();
        }
        for axis in 0..=usize::from(dim) {
            for n in [-9, 8] {
                let mut offsets = [0; 3];
                offsets[axis] = n;
                let err = sample_offset(Some(offset_word(offsets)), dim).unwrap_err();
                assert!(
                    err.to_string().contains("a sample offset past -8..7"),
                    "{err}"
                );
            }
        }
        let err = sample_offset(None, dim).unwrap_err();
        assert!(
            err.to_string()
                .contains("a sample offset that is not constant"),
            "{err}"
        );
    }
}

#[test]
#[cfg(windows)]
fn constant_sample_offsets_read_the_expected_texels() {
    use super::host::{Draw, Host, Root};
    use windows::Win32::Graphics::Direct3D12::*;

    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut validator =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
            .expect("the texture-offset check requires dxil.dll");
    let texels: Vec<_> = (0..32)
        .flat_map(|y| (0..32).map(move |x| [x as f32, y as f32, (y * 32 + x) as f32, 1.0]))
        .collect();
    let samplers = [D3D12_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MaxAnisotropy: 1,
        MaxLOD: f32::MAX,
        ..Default::default()
    }];
    let mut cases = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else { continue };
        for [x, y, z] in [[0, 0, 0], [1, 2, 0], [-1, -2, 0], [-8, 7, 0], [7, -8, 0]] {
            let mut ps = sample_offset(Some(offset_word([x, y, z])), 1).unwrap();
            validator.validate(&mut ps.dxil).unwrap();
            let vs = super::host::stub_vs(&ps).unwrap();
            let (root, pso) = host.pipeline(&vs, &ps).unwrap();
            let got = host
                .draw(
                    &root,
                    &pso,
                    &Draw {
                        roots: vec![
                            (0, Root::Constants(vec![0])),
                            (1, Root::Constants(vec![0; ps.user_data_dwords() as usize])),
                        ],
                        textures: &[(32, 32, &texels)],
                        samplers: &samplers,
                        vertices: &[],
                        stride: 0,
                        size: (4, 4),
                        mesh_groups: 0,
                    },
                )
                .unwrap();
            let want = texels[((16 + y) * 32 + 16 + x) as usize];
            assert_eq!(got.len(), 16);
            assert!(
                got.iter().all(|p| p == &want),
                "{label}: offset({x},{y}): {got:?}, want {want:?}"
            );
            cases += 1;
        }
    }
    assert!(
        cases >= 5,
        "neither hardware nor WARP SM 6.6 device available"
    );
    eprintln!(
        "VICTORY_SAMPLE_OFFSET_RESULT=executed cases={cases} pixels={}",
        cases * 16
    );
}
