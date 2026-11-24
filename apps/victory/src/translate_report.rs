//! Machine-readable offline coverage. Compilation success is not evidence
//! of correct pixels, runtime use, or a performance improvement.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Failure {
    pub phase: String,
    pub reason: String,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Program {
    /// Same first 16 BLAKE3 hex digits used by VICTORY_SKIP_PIPELINES.
    pub shader_hash: String,
    pub container_blake3: String,
    pub program_index: usize,
    pub stage: String,
    pub wave_lanes: u32,
    pub source: String,
    pub emitted: bool,
    /// None means the check was not run; it must not be interpreted as a pass.
    pub validated: Option<bool>,
    pub pipeline_created: Option<bool>,
    pub failures: Vec<Failure>,
}

impl Program {
    pub fn fail(&mut self, phase: &str, reason: String, offset: Option<usize>) {
        self.failures.push(Failure {
            phase: phase.into(),
            reason,
            offset,
        });
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScanFailure {
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Stage {
    pub programs: usize,
    pub emitted: usize,
    pub validated: usize,
    pub validation_failed: usize,
    pub validation_not_run: usize,
    pub pipelines_created: usize,
    pub pipeline_failed: usize,
    pub pipeline_not_run: usize,
    pub programs_with_failures: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct FailureGroup {
    pub stage: String,
    pub phase: String,
    pub reason: String,
    pub programs: usize,
    /// Bounded examples, sorted independently of worker scheduling.
    pub shader_hashes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub schema_version: u32,
    pub victory_version: &'static str,
    pub executable_blake3: Option<String>,
    pub loop_limit: u32,
    pub validation_requested: bool,
    pub pipelines_requested: bool,
    pub containers: usize,
    pub duplicates: usize,
    pub native_programs: usize,
    pub stages: BTreeMap<String, Stage>,
    pub failures: Vec<FailureGroup>,
    pub scan_failures: Vec<ScanFailure>,
    pub programs: Vec<Program>,
    pub interpretation: &'static str,
}

impl Report {
    pub fn new(
        mut programs: Vec<Program>,
        mut scan_failures: Vec<ScanFailure>,
        containers: usize,
        duplicates: usize,
        validation_requested: bool,
        pipelines_requested: bool,
    ) -> Self {
        programs.sort_by(|a, b| {
            (&a.container_blake3, a.program_index, &a.source).cmp(&(
                &b.container_blake3,
                b.program_index,
                &b.source,
            ))
        });
        scan_failures.sort_by(|a, b| (&a.source, &a.reason).cmp(&(&b.source, &b.reason)));
        let mut stages = BTreeMap::<String, Stage>::new();
        let mut groups = BTreeMap::<(String, String, String), (usize, BTreeSet<String>)>::new();
        for p in &programs {
            let stage = stages.entry(p.stage.clone()).or_default();
            stage.programs += 1;
            stage.emitted += usize::from(p.emitted);
            stage.programs_with_failures += usize::from(!p.failures.is_empty());
            match p.validated {
                Some(true) => stage.validated += 1,
                Some(false) => stage.validation_failed += 1,
                None => stage.validation_not_run += 1,
            }
            match p.pipeline_created {
                Some(true) => stage.pipelines_created += 1,
                Some(false) => stage.pipeline_failed += 1,
                None => stage.pipeline_not_run += 1,
            }
            // A program may fail more than one independent phase. Count it
            // once per group, rather than presenting errors as shader totals.
            let unique: BTreeSet<_> = p
                .failures
                .iter()
                .map(|f| (p.stage.clone(), f.phase.clone(), f.reason.clone()))
                .collect();
            for key in unique {
                let (n, examples) = groups.entry(key).or_default();
                *n += 1;
                examples.insert(p.shader_hash.clone());
            }
        }
        let mut failures: Vec<_> = groups
            .into_iter()
            .map(
                |((stage, phase, reason), (programs, hashes))| FailureGroup {
                    stage,
                    phase,
                    reason,
                    programs,
                    shader_hashes: hashes.into_iter().take(5).collect(),
                },
            )
            .collect();
        failures.sort_by(|a, b| {
            b.programs
                .cmp(&a.programs)
                .then_with(|| (&a.stage, &a.phase, &a.reason).cmp(&(&b.stage, &b.phase, &b.reason)))
        });
        Report {
            schema_version: 1,
            victory_version: env!("CARGO_PKG_VERSION"),
            executable_blake3: std::env::current_exe()
                .and_then(std::fs::read)
                .ok()
                .map(|b| blake3::hash(&b).to_hex().to_string()),
            loop_limit: victory_shader::emit::loop_limit(),
            validation_requested,
            pipelines_requested,
            containers,
            duplicates,
            native_programs: programs.len(),
            stages,
            failures,
            scan_failures,
            programs,
            interpretation: "Offline translation coverage only. Groups rank by affected unique programs, not runtime importance. Null checks were not run. Translation, validation and pipeline creation do not prove rendering correctness or performance.",
        }
    }

    pub fn has_failures(&self) -> bool {
        self.native_programs == 0 || !self.failures.is_empty() || !self.scan_failures.is_empty()
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        victory_core::ensure_outside_repo(path)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, self)?;
        writeln!(temp)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)
            .map_err(|e| e.error)
            .with_context(|| {
                format!(
                    "saving report {} (choose a new filename if it exists)",
                    path.display()
                )
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(hash: &str) -> Program {
        Program {
            shader_hash: hash.into(),
            container_blake3: hash.into(),
            program_index: 0,
            stage: "Cs".into(),
            wave_lanes: 64,
            source: "synthetic".into(),
            emitted: true,
            validated: None,
            pipeline_created: None,
            failures: Vec::new(),
        }
    }

    #[test]
    fn absent_checks_are_not_successes() {
        let r = Report::new(vec![program("a")], vec![], 1, 0, false, false);
        let s = &r.stages["Cs"];
        assert_eq!((s.emitted, s.validated, s.pipelines_created), (1, 0, 0));
        assert_eq!((s.validation_not_run, s.pipeline_not_run), (1, 1));
        assert!(
            serde_json::to_string(&r)
                .unwrap()
                .contains("\"validated\":null")
        );
    }

    #[test]
    fn independent_phase_failures_do_not_double_count_programs() {
        let mut p = program("b");
        p.fail("resources", "untracked address".into(), Some(4));
        p.fail("structure", "indirect jump".into(), Some(8));
        let mut q = program("a");
        q.fail("resources", "untracked address".into(), Some(16));
        let r = Report::new(vec![p, q], vec![], 2, 0, false, false);
        assert_eq!(r.stages["Cs"].programs_with_failures, 2);
        assert_eq!(r.failures[0].programs, 2);
        assert_eq!(r.failures[0].shader_hashes, ["a", "b"]);
        assert!(r.has_failures());
    }

    #[test]
    fn empty_and_incomplete_scans_fail_the_gate() {
        assert!(Report::new(vec![], vec![], 0, 0, false, false).has_failures());
        let r = Report::new(
            vec![program("a")],
            vec![ScanFailure {
                source: "missing".into(),
                reason: "unreadable".into(),
            }],
            1,
            0,
            false,
            false,
        );
        assert!(r.has_failures());
    }

    #[test]
    fn report_round_trips_and_does_not_replace_existing_files() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("report.json");
        let r = Report::new(vec![program("a")], vec![], 1, 0, false, false);
        r.write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert!(r.write(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
