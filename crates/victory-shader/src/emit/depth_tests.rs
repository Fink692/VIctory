//! Conservative depth with a position input, authored from three GFX10
//! instructions. Covers the host validator's interpolation requirement.

use super::container::SigElement;
use crate::native::{HwStage, NativeProgram, StageIo};
use crate::{Container, FourCC, io, ir, resource, structure};

#[test]
fn conservative_depth_uses_centroid_position() {
    #[cfg(windows)]
    let mut validator =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
            .expect("conservative depth regression requires dxil.dll");
    for (kind, name, interpolation) in [
        (17, "SV_Depth", 4),
        (18, "SV_DepthLessEqual", 5),
        (19, "SV_DepthGreaterEqual", 5),
    ] {
        // Position.z arrives in v0. Write it as depth.
        let words: [u32; 3] = [0xf800_1881, 0, 0xbf81_0000];
        let native = NativeProgram {
            stage: HwStage::Ps,
            wave64: false,
            vgprs: 1,
            layout: vec![],
            used: vec![],
            io: StageIo::Ps {
                inputs: vec![],
                input_addr: 1 << 10,
                db_shader_control: 1,
                patches: vec![],
            },
            code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
        };
        let position = SigElement {
            name: "SV_Position".into(),
            index: 0,
            kind: 3,
            comp: 9,
            interp: 0,
            row: Some(0),
            col: 0,
            cols: 4,
            usage: 4,
            half: false,
        };
        let depth = SigElement {
            name: name.into(),
            index: 0,
            kind,
            comp: 9,
            interp: 0,
            row: None,
            col: 0,
            cols: 1,
            usage: 1,
            half: false,
        };
        let ins = super::container::signature(&[position], true);
        let outs = super::container::signature(&[depth], false);
        let container = crate::dxbc::build(&[(FourCC::ISG1, &ins), (FourCC::OSG1, &outs)]);
        let container = Container::parse(&container).unwrap();
        let p = ir::build(&native.code, native.wave64, native.vgprs).unwrap();
        let st = structure::structure(&p).unwrap();
        let res = resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
        let io = io::raise(&p, &native).unwrap();
        let shader = super::emit(&container, &native, &p, &st, &io, &res).unwrap();
        let c = Container::parse(&shader.dxil).unwrap();
        let psv = c.part(FourCC::PSV0).unwrap().data;
        let word = |at| u32::from_le_bytes(psv[at..at + 4].try_into().unwrap()) as usize;
        // PSV0: runtime info, bindings, strings, indices, then 16-byte
        // signature records; the first input's interpolation is byte13.
        let mut at = 4 + word(0);
        assert_eq!(word(at), 0, "no resources in this fixture");
        at += 4;
        at += 4 + word(at);
        at += 4 + 4 * word(at);
        assert_eq!(word(at), 16);
        at += 4;
        assert_eq!(psv[at + 11], 3, "first input is position");
        assert_eq!(psv[at + 13], interpolation, "{name}");
        if let Some(exe) = std::env::var_os("VICTORY_DXV") {
            super::atomic_tests::validate_dxv(&exe, &shader.dxil);
        }
        #[cfg(windows)]
        validator
            .validate(&mut shader.dxil.clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    #[cfg(windows)]
    eprintln!("VICTORY_DEPTH_VALIDATION_RESULT=validated containers=3");
}
