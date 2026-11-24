//! The offline batch: every shader found goes through the translator, and
//! what it cannot handle yet is tallied (decision 0003).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use victory_shader::emit;
#[cfg(windows)]
use victory_shader::emit::host::{self, Host};
#[cfg(windows)]
use victory_shader::emit::validator::Validator;
use victory_shader::structure::{self, Atom, LaneExit};
use victory_shader::{
    Container, FourCC, HwStage, NativeProgram, RootType, ShaderError, ir, native_programs,
};
use victory_shader::{io, resource};

use crate::shader::{MAX_FILE, Source, sources};
use crate::translate_report::{Program as ProgramReport, Report, ScanFailure};

#[derive(Default)]
struct Tally {
    containers: usize,
    duplicates: usize,
    programs: usize,
    translated: usize,
    instructions: usize,
    structured: usize,
    vector: usize,
    guarded: usize,
    /// Guards that read a mask the pass cannot see into.
    opaque: usize,
    lane_branches: usize,
    uniform_branches: usize,
    tracked: usize,
    loaded: usize,
    built: usize,
    constant: usize,
    /// Descriptors and scalar accesses with a computed address term.
    dynamic: usize,
    scalar: usize,
    used: usize,
    /// Used-resource entries whose root parameter no descriptor traces
    /// to, by resource and root type.
    untraced_used: BTreeMap<String, usize>,
    /// Root parameters descriptors trace to that no used entry names.
    unlisted: usize,
    /// Vertex and pixel programs, and those whose I/O was raised.
    io_programs: usize,
    io_raised: usize,
    interpolated: usize,
    vertex_values: usize,
    /// Exports by target kind.
    exports: BTreeMap<&'static str, usize>,
    /// Exports still packed as f16 pairs after the export patch.
    compressed: usize,
    /// Vertex and pixel programs every earlier stage handled, and how far
    /// emission got with them.
    emit_programs: usize,
    emitted: usize,
    validated: usize,
    pipelines: usize,
    /// The same for compute programs.
    cs_programs: usize,
    cs_emitted: usize,
    cs_validated: usize,
    cs_pipelines: usize,
    /// The same for mesh programs.
    ms_programs: usize,
    ms_emitted: usize,
    ms_validated: usize,
    ms_pipelines: usize,
    /// Reason → programs stopped by it, and the first one.
    failed: BTreeMap<String, (usize, String)>,
    /// With `--list`: one line per program and outcome.
    list: bool,
    lines: Vec<String>,
    records: Vec<ProgramReport>,
    scan_failures: Vec<ScanFailure>,
}

impl Tally {
    fn fail(&mut self, reason: String, example: impl FnOnce() -> String) {
        if self.list {
            let e = example();
            self.lines.push(format!("{e}  {reason}"));
            self.failed.entry(reason).or_insert((0, e)).0 += 1;
            return;
        }
        self.failed
            .entry(reason)
            .or_insert_with(|| (0, example()))
            .0 += 1;
    }

    fn merge(&mut self, other: Tally) {
        self.containers += other.containers;
        self.duplicates += other.duplicates;
        self.programs += other.programs;
        self.translated += other.translated;
        self.instructions += other.instructions;
        self.structured += other.structured;
        self.vector += other.vector;
        self.guarded += other.guarded;
        self.opaque += other.opaque;
        self.lane_branches += other.lane_branches;
        self.uniform_branches += other.uniform_branches;
        self.tracked += other.tracked;
        self.loaded += other.loaded;
        self.built += other.built;
        self.constant += other.constant;
        self.dynamic += other.dynamic;
        self.scalar += other.scalar;
        self.used += other.used;
        for (k, n) in other.untraced_used {
            *self.untraced_used.entry(k).or_default() += n;
        }
        self.unlisted += other.unlisted;
        self.io_programs += other.io_programs;
        self.io_raised += other.io_raised;
        self.interpolated += other.interpolated;
        self.vertex_values += other.vertex_values;
        self.compressed += other.compressed;
        self.emit_programs += other.emit_programs;
        self.emitted += other.emitted;
        self.validated += other.validated;
        self.pipelines += other.pipelines;
        self.cs_programs += other.cs_programs;
        self.cs_emitted += other.cs_emitted;
        self.cs_validated += other.cs_validated;
        self.cs_pipelines += other.cs_pipelines;
        self.ms_programs += other.ms_programs;
        self.ms_emitted += other.ms_emitted;
        self.ms_validated += other.ms_validated;
        self.ms_pipelines += other.ms_pipelines;
        for (k, n) in other.exports {
            *self.exports.entry(k).or_default() += n;
        }
        self.lines.extend(other.lines);
        self.records.extend(other.records);
        self.scan_failures.extend(other.scan_failures);
        for (reason, (n, example)) in other.failed {
            self.failed.entry(reason).or_insert((0, example)).0 += n;
        }
    }
}

/// Emitted shaders by label, shared between threads.
type Kept = std::sync::Arc<Mutex<Vec<(String, emit::HostShader)>>>;

/// The optional checks after emission, one set per thread.
struct Checks {
    /// Shaders named in `VICTORY_PAIRS` (`vs+ps,...`, label substrings),
    /// kept to build those pipelines after the walk.
    keep: Option<Kept>,
    #[cfg(windows)]
    validator: Option<Validator>,
    #[cfg(windows)]
    host: Option<(Host, emit::HostShader)>,
}

impl Checks {
    fn new(
        validate: bool,
        pipelines: bool,
        dll: Option<&Path>,
        keep: Option<Kept>,
    ) -> Result<Self> {
        #[cfg(not(windows))]
        {
            let _ = dll;
            anyhow::ensure!(
                !validate && !pipelines && keep.is_none(),
                "DXIL validation and host pipeline checks require Windows"
            );
            Ok(Self { keep })
        }
        #[cfg(windows)]
        Ok(Self {
            keep,
            validator: validate
                .then(|| Validator::load(dll).map_err(anyhow::Error::msg))
                .transpose()?,
            host: pipelines
                .then(|| {
                    Host::new(false)
                        .map(|h| (h, host::stub_ps()))
                        .map_err(anyhow::Error::msg)
                })
                .transpose()?,
        })
    }
}

pub(crate) struct Options {
    pub validate: bool,
    pub pipelines: bool,
    pub list: bool,
    pub jobs: Option<usize>,
    pub report: Option<PathBuf>,
    pub fail_on_error: bool,
}

pub fn run(paths: Vec<PathBuf>, options: Options) -> Result<()> {
    let Options {
        validate,
        pipelines,
        list,
        jobs,
        report,
        fail_on_error,
    } = options;
    anyhow::ensure!(jobs != Some(0), "--jobs must be at least 1");
    if let Some(path) = &report {
        victory_core::ensure_outside_repo(path)?;
        anyhow::ensure!(
            !path.exists(),
            "report {} already exists; choose a new filename",
            path.display()
        );
    }
    // Dumps are derived data too; reject a source-tree destination before
    // any worker can create files, including on validation failure.
    if let Some(dir) = std::env::var_os("VICTORY_EMIT_DUMP") {
        victory_core::ensure_outside_repo(Path::new(&dir))?;
        std::fs::create_dir_all(dir)?;
    }
    let paths = crate::default_paths(paths)?;
    #[cfg(windows)]
    let dll = validate.then(Validator::default_path).flatten();
    #[cfg(not(windows))]
    let dll: Option<PathBuf> = None;
    Checks::new(validate, pipelines, dll.as_deref(), None)?;
    #[cfg(windows)]
    if validate {
        println!(
            "validator    {}",
            dll.as_ref()
                .map_or("dxil.dll".into(), |p| p.display().to_string())
        );
    }
    let pairs = parse_pairs(&std::env::var("VICTORY_PAIRS").unwrap_or_default())?;
    #[cfg(not(windows))]
    anyhow::ensure!(
        pairs.is_empty(),
        "VICTORY_PAIRS requires Windows host pipeline checks"
    );
    let kept = std::sync::Arc::new(Mutex::new(Vec::new()));
    let threads =
        jobs.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
    let seen = Mutex::new(HashSet::new());
    let total = Mutex::new(Tally::default());
    for root in &paths {
        for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    lock(&total).scan_failures.push(ScanFailure {
                        source: e.path().unwrap_or(root).display().to_string(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let data = match entry.metadata().map_err(anyhow::Error::from).and_then(|m| {
                anyhow::ensure!(
                    m.len() <= MAX_FILE,
                    "file exceeds {MAX_FILE}-byte scan limit"
                );
                std::fs::read(entry.path()).map_err(anyhow::Error::from)
            }) {
                Ok(data) => data,
                Err(e) => {
                    lock(&total).scan_failures.push(ScanFailure {
                        source: entry.path().display().to_string(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            // The archive scanner deliberately ignores invalid embedded
            // magic strings. A file starting with DXBC is a direct container,
            // though: do not let a corrupt one disappear from a mixed scan.
            if data.starts_with(b"DXBC")
                && let Err(e) = Container::parse(&data)
            {
                lock(&total).scan_failures.push(ScanFailure {
                    source: entry.path().display().to_string(),
                    reason: format!("invalid DXBC container: {e}"),
                });
                continue;
            }
            let jobs = sources(&data);
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| -> Result<()> {
                let mut workers = Vec::new();
                for _ in 0..threads.min(jobs.len()) {
                    workers.push(s.spawn(|| -> Result<()> {
                        let mut t = Tally {
                            list,
                            ..Tally::default()
                        };
                        let mut checks = Checks::new(
                            validate,
                            pipelines,
                            dll.as_deref(),
                            (!pairs.is_empty()).then(|| kept.clone()),
                        )
                        .context("initializing requested shader checks")?;
                        while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                            translate(job, entry.path(), &seen, &mut t, &mut checks);
                        }
                        lock(&total).merge(t);
                        Ok(())
                    }));
                }
                for worker in workers {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("shader worker panicked"))??;
                }
                Ok(())
            })?;
        }
    }
    #[allow(unused_mut)]
    let mut t = total.into_inner().unwrap_or_else(|e| e.into_inner());
    println!(
        "containers   {} ({} duplicates skipped)",
        t.containers, t.duplicates
    );
    println!(
        "programs     {} translated of {} ({} IR instructions)",
        t.translated, t.programs, t.instructions
    );
    println!(
        "structured   {} of {} ({} vector instructions, {} guarded, {} on opaque masks; \
         branches {} per lane, {} uniform)",
        t.structured,
        t.translated,
        t.vector,
        t.guarded,
        t.opaque,
        t.lane_branches,
        t.uniform_branches
    );
    println!(
        "resources    {} of {} tracked (descriptors {} loaded, {} built, {} constant; \
         {} scalar accesses; {} with a computed term)",
        t.tracked, t.translated, t.loaded, t.built, t.constant, t.scalar, t.dynamic
    );
    println!(
        "  used-resource entries {}: {} not traced {:?}; {} traced parameters not listed",
        t.used,
        t.untraced_used.values().sum::<usize>(),
        t.untraced_used,
        t.unlisted
    );
    println!(
        "I/O          {} of {} vertex and pixel programs raised ({} interpolations, \
         {} vertex values; exports {:?}, {} still f16 pairs)",
        t.io_raised, t.io_programs, t.interpolated, t.vertex_values, t.exports, t.compressed
    );
    println!(
        "DXIL         {} emitted of {} vertex and pixel programs; {} validated, {} pipelines",
        t.emitted, t.emit_programs, t.validated, t.pipelines
    );
    println!(
        "             {} emitted of {} compute programs; {} validated, {} pipelines",
        t.cs_emitted, t.cs_programs, t.cs_validated, t.cs_pipelines
    );
    println!(
        "             {} emitted of {} mesh programs; {} validated, {} pipelines",
        t.ms_emitted, t.ms_programs, t.ms_validated, t.ms_pipelines
    );
    #[cfg(windows)]
    if !pairs.is_empty() {
        let host = Host::new(false).map_err(anyhow::Error::msg)?;
        let kept = lock(&kept);
        let find = |name: &str| -> std::result::Result<&emit::HostShader, String> {
            let mut found = kept.iter().filter(|(l, _)| l.contains(name));
            let (_, shader) = found.next().ok_or_else(|| format!("{name}: not emitted"))?;
            if found.next().is_some() {
                return Err(format!("{name}: ambiguous label; use a hash and stage"));
            }
            Ok(shader)
        };
        for (v, p) in &pairs {
            let result = match (find(v), find(p)) {
                (Ok(vs), Ok(ps)) if vs.stage == HwStage::Ms => host.mesh_pipeline(vs, ps).map(drop),
                (Ok(vs), Ok(ps)) => host.pipeline(vs, ps).map(drop),
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            match result {
                Ok(()) => println!("pair {v} + {p}: pipeline created"),
                Err(e) => {
                    println!("pair {v} + {p}: {e}");
                    t.scan_failures.push(ScanFailure {
                        source: format!("pair {v} + {p}"),
                        reason: e,
                    });
                }
            }
        }
    }
    let summary = Report::new(
        t.records,
        t.scan_failures,
        t.containers,
        t.duplicates,
        validate,
        pipelines,
    );
    if let Some(path) = &report {
        summary.write(path)?;
        println!("report       {}", path.display());
    }
    for f in &summary.scan_failures {
        eprintln!("scan failed: {}: {}", f.source, f.reason);
    }
    let mut lines = t.lines;
    lines.sort();
    for l in lines {
        println!("{l}");
    }
    let mut failed: Vec<_> = t.failed.into_iter().collect();
    failed.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (reason, (n, example)) in failed {
        println!("  {n:>7}  {reason}  (e.g. {example})");
    }
    anyhow::ensure!(
        !fail_on_error || !summary.has_failures(),
        "translation checks failed or the scan was incomplete (see report and diagnostics)"
    );
    Ok(())
}

fn parse_pairs(value: &str) -> Result<Vec<(String, String)>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|entry| {
            let (vertex, pixel) = entry.split_once('+').with_context(|| {
                format!("invalid VICTORY_PAIRS entry {entry:?}: expected vertex+pixel labels")
            })?;
            anyhow::ensure!(
                !vertex.trim().is_empty() && !pixel.trim().is_empty() && !pixel.contains('+'),
                "invalid VICTORY_PAIRS entry {entry:?}: expected two nonempty labels"
            );
            Ok((vertex.trim().into(), pixel.trim().into()))
        })
        .collect()
}

fn translate(
    source: &Source,
    path: &Path,
    seen: &Mutex<HashSet<blake3::Hash>>,
    t: &mut Tally,
    checks: &mut Checks,
) {
    let example = || format!("{}{}", path.display(), source.label());
    let scan_failure = |t: &mut Tally, reason: String| {
        t.scan_failures.push(ScanFailure {
            source: example(),
            reason: reason.clone(),
        });
        t.fail(reason, example);
    };
    let Some(bytes) = source.bytes() else {
        return scan_failure(t, "corrupt record".into());
    };
    t.containers += 1;
    let container = match Container::parse(&bytes) {
        Ok(c) => c,
        Err(e) => return scan_failure(t, format!("container: {e}")),
    };
    let Some(dxil) = container.part(FourCC::DXIL).map(|p| p.data) else {
        return scan_failure(t, "no DXIL part (SM5 or older)".into());
    };
    // The DXIL part alone is not an identity: emission also reads the
    // container's signatures. Exclude trailing archive bytes from the hash.
    let hash = container_hash(&bytes);
    if !lock(seen).insert(hash) {
        t.duplicates += 1;
        return;
    }
    let programs = match native_programs(dxil) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return scan_failure(t, "no native programs in container".into()),
        Err(e) => return scan_failure(t, format!("native: {e}")),
    };
    for (index, p) in programs.into_iter().enumerate() {
        t.programs += 1;
        let full_hash = hash.to_hex().to_string();
        let mut record = ProgramReport {
            shader_hash: full_hash[..16].into(),
            container_blake3: full_hash,
            program_index: index,
            stage: format!("{:?}", p.stage),
            wave_lanes: if p.wave64 { 64 } else { 32 },
            source: example(),
            emitted: false,
            validated: None,
            pipeline_created: None,
            failures: Vec::new(),
        };
        analyze(&container, &p, t, checks, &mut record);
        t.records.push(record);
    }
}

/// Uses the same canonical container bytes as runtime pipeline creation.
fn container_hash(bytes: &[u8]) -> blake3::Hash {
    blake3::hash(victory_shader::dxbc::canonical_bytes(bytes))
}

fn record_failure(t: &mut Tally, record: &mut ProgramReport, phase: &str, error: ShaderError) {
    let (reason, offset) = match error {
        ShaderError::Unsupported { what, offset } => (what.to_owned(), Some(offset)),
        ShaderError::Isa { reason, offset } => (reason.to_owned(), Some(offset)),
        e => (e.to_string(), None),
    };
    record.fail(phase, reason.clone(), offset);
    t.fail(format!("{phase}: {reason}"), || {
        format!(
            "{} {} {} at {:?}",
            record.shader_hash, record.source, record.stage, offset
        )
    });
}

fn analyze(
    container: &Container,
    p: &NativeProgram,
    t: &mut Tally,
    checks: &mut Checks,
    record: &mut ProgramReport,
) {
    let program = match ir::build(&p.patched_code(), p.wave64, p.vgprs) {
        Ok(program) => program,
        Err(e) => return record_failure(t, record, "ir", e),
    };
    t.translated += 1;
    t.instructions += program.blocks.iter().map(|b| b.insts.len()).sum::<usize>();
    // Per-lane descriptor joins need the lane conditions before resources
    // are traced; preserve the existing report ordering below.
    let structure_result = structure::structure(&program);
    let res = match resource::track(&program, &p.layout, p.stage, structure_result.as_ref().ok()) {
        Ok(r) => {
            tally_resources(p, &r, t);
            Some(r)
        }
        Err(e) => {
            record_failure(t, record, "resources", e);
            None
        }
    };
    let graphics = matches!(p.stage, HwStage::Vs | HwStage::Ps);
    let raised = if graphics || matches!(p.stage, HwStage::Cs | HwStage::Ms) {
        t.io_programs += usize::from(graphics);
        match io::raise(&program, p) {
            Ok(r) => {
                if graphics {
                    tally_io(&r, t);
                }
                Some(r)
            }
            Err(e) => {
                record_failure(t, record, "io", e);
                None
            }
        }
    } else {
        record_failure(
            t,
            record,
            "stage",
            ShaderError::Unsupported {
                offset: 0,
                what: "merged geometry or tessellation stage",
            },
        );
        None
    };
    let structured = match structure_result {
        Ok(s) => {
            tally_structured(&program, &s, t);
            Some(s)
        }
        Err(e) => {
            record_failure(t, record, "structure", e);
            None
        }
    };
    if let (Some(r), Some(i), Some(s)) = (res, raised, structured) {
        *counter(t, p.stage, 0) += 1;
        match emit::emit(container, p, &program, &s, &i, &r) {
            Ok(shader) => {
                *counter(t, p.stage, 1) += 1;
                record.emitted = true;
                check(shader, t, checks, record);
            }
            Err(e) => record_failure(t, record, "emit", e),
        }
    }
}

/// Validates an emitted shader if asked to. Check initialization failures
/// propagate from the worker; an unavailable validator is never a pass.
fn check(
    #[allow(unused_mut)] mut shader: emit::HostShader,
    t: &mut Tally,
    checks: &mut Checks,
    record: &mut ProgramReport,
) {
    let label = format!("{} {} {}", record.shader_hash, record.source, record.stage);
    #[cfg(windows)]
    if let Some(v) = checks.validator.as_mut() {
        match v.validate(&mut shader.dxil) {
            Ok(()) => {
                record.validated = Some(true);
                *counter(t, shader.stage, 2) += 1;
                if t.list {
                    t.lines.push(format!("{label}  validated"));
                }
            }
            Err(text) => {
                record.validated = Some(false);
                let first = text
                    .lines()
                    .find(|l| l.contains("error"))
                    .unwrap_or(text.lines().next().unwrap_or("validator failed"))
                    .to_owned();
                record.fail("validator", first.clone(), None);
                t.fail(format!("validator: {first}"), || label.clone());
                dump_shader(&shader, t, record, Some(&text));
                return;
            }
        }
    }
    if let Some(keep) = &checks.keep {
        lock(keep).push((label.clone(), shader.clone()));
    }
    if t.list {
        dump_shader(&shader, t, record, None);
    }
    #[cfg(windows)]
    if let Some((h, stub_ps)) = &checks.host {
        let made = match shader.stage {
            HwStage::Vs => h.pipeline(&shader, stub_ps).map(drop),
            HwStage::Cs => h.compute_pipeline(&shader).map(drop),
            HwStage::Ms => h.mesh_pipeline(&shader, stub_ps).map(drop),
            _ => host::stub_vs(&shader).and_then(|vs| h.pipeline(&vs, &shader).map(drop)),
        };
        match made {
            Ok(()) => {
                record.pipeline_created = Some(true);
                *counter(t, shader.stage, 3) += 1;
                if t.list {
                    t.lines.push(format!("{label}  pipeline created"));
                }
            }
            Err(e) => {
                record.pipeline_created = Some(false);
                record.fail("pipeline", e.clone(), None);
                t.fail(format!("pipeline: {e}"), || label.clone());
            }
        }
    }
    if t.list && record.validated.is_none() && record.pipeline_created.is_none() {
        t.lines.push(format!(
            "{label}  emitted (validation and pipeline creation not run)"
        ));
    }
}

fn dump_shader(
    shader: &emit::HostShader,
    t: &mut Tally,
    record: &ProgramReport,
    error: Option<&str>,
) {
    let Some(dir) = std::env::var_os("VICTORY_EMIT_DUMP") else {
        return;
    };
    let dir = Path::new(&dir);
    let result = (|| -> Result<()> {
        victory_core::ensure_outside_repo(dir)?;
        let stem = format!(
            "{}_{}_{}",
            record.shader_hash, record.stage, record.program_index
        );
        std::fs::write(dir.join(format!("{stem}.dxil")), &shader.dxil)?;
        if let Some(error) = error {
            std::fs::write(dir.join(format!("{stem}.txt")), error)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        t.scan_failures.push(ScanFailure {
            source: dir.display().to_string(),
            reason: format!("writing shader diagnostics: {e}"),
        });
    }
}

/// Emission counts of `stage`'s kind: 0 programs, 1 emitted, 2 validated,
/// 3 pipelines.
fn counter(t: &mut Tally, stage: HwStage, k: usize) -> &mut usize {
    let [a, b, c, d] = match stage {
        HwStage::Cs => [
            &mut t.cs_programs,
            &mut t.cs_emitted,
            &mut t.cs_validated,
            &mut t.cs_pipelines,
        ],
        HwStage::Ms => [
            &mut t.ms_programs,
            &mut t.ms_emitted,
            &mut t.ms_validated,
            &mut t.ms_pipelines,
        ],
        _ => [
            &mut t.emit_programs,
            &mut t.emitted,
            &mut t.validated,
            &mut t.pipelines,
        ],
    };
    [a, b, c, d].into_iter().nth(k).expect("0 to 3")
}

fn tally_io(r: &io::Io, t: &mut Tally) {
    t.io_raised += 1;
    for (_, i) in &r.interps {
        match i {
            io::Interp::Interpolated { .. } => t.interpolated += 1,
            io::Interp::Vertex { .. } => t.vertex_values += 1,
        }
    }
    for e in &r.exports {
        let kind = match e.target {
            io::Target::Color(_) => "color",
            io::Target::Depth => "depth",
            io::Target::Null => "null",
            io::Target::Position(_) => "position",
            io::Target::Param { .. } => "param",
            io::Target::Unread => "unread param",
            io::Target::Primitive => "primitive",
            io::Target::PrimitiveParam { .. } => "primitive param",
        };
        *t.exports.entry(kind).or_default() += 1;
        t.compressed += usize::from(e.compressed);
    }
}

fn tally_resources(p: &NativeProgram, r: &resource::Resources, t: &mut Tally) {
    t.tracked += 1;
    t.scalar += r.scalar.len();
    t.dynamic += r.scalar.iter().filter(|s| s.1.dynamic.is_some()).count();
    let address = |load| r.scalar.iter().find(|s| s.0 == load).map(|s| s.1);
    let mut traced = HashSet::new();
    for leaf in r.descriptors.iter().flat_map(|d| d.source.leaves()) {
        let a = match *leaf {
            resource::Source::Loaded { load, .. } | resource::Source::Fetched { load, .. } => {
                t.loaded += 1;
                address(load)
            }
            resource::Source::Built { address, .. } => {
                t.built += 1;
                t.dynamic += usize::from(address.dynamic.is_some());
                Some(address)
            }
            resource::Source::Constant(_) => {
                t.constant += 1;
                None
            }
            resource::Source::Phi { .. }
            | resource::Source::Select { .. }
            | resource::Source::Lanes { .. }
            | resource::Source::LanePhi { .. } => unreachable!("leaves are no joins"),
        };
        // Heaps hold dynamic resources, which no used entry lists; code
        // tables are no root parameter.
        if let Some(a) = a
            && a.param != resource::CODE
            && !matches!(
                p.layout[a.param].root,
                RootType::ResourceHeap | RootType::SamplerHeap
            )
        {
            traced.insert(p.layout[a.param].user_data);
        }
    }
    // Root constants are listed as a CBV, read straight from the SGPRs.
    traced.extend(
        r.params
            .iter()
            .map(|&(_, param, _)| &p.layout[param])
            .filter(|l| l.root == RootType::Constants)
            .map(|l| l.user_data),
    );
    let listed: HashSet<u8> = p.used.iter().map(|u| u.user_data).collect();
    t.used += p.used.len();
    // Immediate samplers are literals in the code: the distinct sampler
    // values (decoded) are matched against the listed entries, which do not
    // say which value is whose.
    let mut values: Vec<emit::StaticSampler> = Vec::new();
    for d in r
        .descriptors
        .iter()
        .filter(|d| d.role == resource::Role::Sampler)
    {
        for leaf in d.source.leaves() {
            if let resource::Source::Constant(words) = leaf
                && let Ok(w) = <[u32; 4]>::try_from(&words[..words.len().min(4)])
            {
                let s = emit::StaticSampler::from_words(w);
                if !values.contains(&s) {
                    values.push(s);
                }
            }
        }
    }
    // Engines often declare equal samplers at several registers, so equal
    // values may stand for several entries, but never more values than
    // entries.
    let immediate = p
        .used
        .iter()
        .filter(|u| u.root == RootType::ImmediateSampler)
        .count();
    let samplers_match = !values.is_empty() && values.len() <= immediate;
    let matched = |u: &&victory_shader::UsedResource| {
        if u.root == RootType::ImmediateSampler {
            samplers_match
        } else {
            traced.contains(&u.user_data)
        }
    };
    for u in p.used.iter().filter(|u| !matched(u)) {
        *t.untraced_used
            .entry(format!("{:?} via {:?}", u.kind, u.root))
            .or_default() += 1;
    }
    t.unlisted += traced.difference(&listed).count();
}

fn tally_structured(program: &ir::Program, s: &structure::Structured, t: &mut Tally) {
    t.structured += 1;
    for (block, guards) in program.blocks.iter().zip(&s.guards) {
        t.vector += block
            .insts
            .iter()
            .filter(|i| i.read(ir::Reg::S(ir::EXEC)).is_some())
            .count();
        for &g in guards.iter().flatten() {
            t.guarded += 1;
            let support = s.bools.support(g);
            t.opaque += usize::from(support.iter().any(|a| matches!(a, Atom::Opaque(_))));
        }
    }
    for exit in &s.exits {
        if let LaneExit::Branch { when, .. } = *exit {
            if s.bools
                .support(when)
                .iter()
                .all(|a| matches!(a, Atom::Uniform(_)))
            {
                t.uniform_branches += 1;
            } else {
                t.lane_branches += 1;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(report: Option<PathBuf>) -> Options {
        Options {
            validate: false,
            pipelines: false,
            list: false,
            jobs: Some(1),
            report,
            fail_on_error: true,
        }
    }

    #[test]
    fn zero_workers_cannot_silently_report_success() {
        let mut opts = options(None);
        opts.jobs = Some(0);
        assert!(
            run(vec![], opts)
                .unwrap_err()
                .to_string()
                .contains("--jobs")
        );
    }

    #[test]
    fn empty_scan_writes_evidence_then_fails_regression_gate() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join("report.json");
        assert!(run(vec![input.path().into()], options(Some(path.clone()))).is_err());
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["native_programs"], 0);
        assert!(report["programs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn missing_input_is_not_an_empty_successful_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        assert!(
            run(
                vec![dir.path().join("missing")],
                options(Some(path.clone()))
            )
            .is_err()
        );
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["scan_failures"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn malformed_direct_container_is_reported_but_embedded_magic_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.dxbc");
        let ordinary = dir.path().join("ordinary.txt");
        std::fs::write(&broken, b"DXBC truncated header").unwrap();
        std::fs::write(&ordinary, b"an unrelated embedded DXBC string").unwrap();
        let path = dir.path().join("report.json");
        assert!(run(vec![broken, ordinary], options(Some(path.clone()))).is_err());
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let failures = report["scan_failures"].as_array().unwrap();
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0]["reason"]
                .as_str()
                .unwrap()
                .contains("invalid DXBC")
        );
    }

    #[test]
    fn explicit_pipeline_pairs_cannot_be_silently_ignored() {
        assert!(parse_pairs(" \t").unwrap().is_empty());
        assert_eq!(
            parse_pairs(" vertex + pixel , mesh+fragment ").unwrap(),
            vec![
                ("vertex".into(), "pixel".into()),
                ("mesh".into(), "fragment".into())
            ]
        );
        for value in ["vertex", "vertex+", "+pixel", "a+b+c", "a+b,", ",a+b"] {
            assert!(parse_pairs(value).is_err(), "{value}");
        }
    }

    #[test]
    fn reports_cannot_be_written_into_the_source_tree() {
        let inside = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("report.json");
        assert!(
            run(vec![], options(Some(inside)))
                .unwrap_err()
                .to_string()
                .contains("source tree")
        );
    }

    #[test]
    fn container_identity_includes_metadata_but_not_archive_tail() {
        // An empty synthetic DXBC header. No game-derived bytes.
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(b"DXBC");
        bytes[20] = 1;
        bytes[24..28].copy_from_slice(&32u32.to_le_bytes());
        Container::parse(&bytes).unwrap();
        let original = container_hash(&bytes);
        bytes.extend_from_slice(b"unrelated archive data");
        assert_eq!(container_hash(&bytes), original);
        bytes[4] = 1;
        assert_ne!(container_hash(&bytes), original);
    }

    #[cfg(not(windows))]
    #[test]
    fn requested_host_checks_fail_explicitly_when_unavailable() {
        assert!(Checks::new(true, false, None, None).is_err());
        assert!(Checks::new(false, true, None, None).is_err());
        assert!(Checks::new(false, false, None, None).is_ok());
    }
}
