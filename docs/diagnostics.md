# Windows validation and rendering diagnostics

The next rendering milestone is a recognizable first world scene without a
device reset. Passing a build, translating a shader or keeping a process
alive does not establish that milestone. The tools below keep those results
separate so fixes can be compared against the same scenario.

## Prerequisites

Use Windows x64, the MSVC C++ build tools and Windows SDK, the repository's
pinned Rust toolchain, and PowerShell 7.2 or newer (`pwsh`). The strict GPU
test gate also needs the SDK's `x64/dxil.dll`, sibling `dxc.exe`, and a WARP adapter able to
create the synthetic shader-model 6.6 pipeline. A missing prerequisite is a
failure of this gate, rather than a passing skipped GPU test.

## Check a source change without game files

From the repository root:

```powershell
pwsh -NoProfile -File scripts/Test-DiagnosticsHarness.ps1
pwsh -NoProfile -File scripts/Test-Victory.ps1 -RequireGpu
```

The first command tests the process runner with stand-in child processes:
argument quoting, environment isolation, both output pipes, nonzero exits,
timeouts, refusing log replacement and keeping output outside source trees.
It needs neither Rust nor a GPU and also runs on Linux with PowerShell 7.2+.

The second command checks formatting of complete changed Rust files. It
runs required synthetic DXIL and WARP tests covering pipeline creation,
query readback, query-to-copy visibility across command lists and indirect
budgets. It also runs the workspace tests with all features, checks Clippy,
builds release and diagnostics executables, and checks the asset policy.
The 88-container image-atomic validation matrix runs in its own process.
Two separate atomic64 execution checks then record `coverage.status`, adapter
count and executed case count in `checks.json`: unguarded operations and
guarded operations, each with 352 cases per capable adapter. Guarded execution
also needs the measured wave-size capability required by the emitted shader.
An unsupported guarded case does not prevent testing all unguarded operations.
A capability-based `unsupported` result provides no execution evidence for
that check; a native crash, missing coverage marker or failed case fails the
gate. These isolated checks request the D3D12 debug layer and run immediately
after the basic WARP pipeline check. Their separate logs and flushed phase
markers distinguish validator, pipeline and dispatch failures before the
parallel workspace suite runs.

The shader-identity check compiles two authored programs with the SDK's DXC
and compares their `HASH` payloads to our bitcode digest. The LDS matrix
independently validates 168 byte/halfword shared-memory
programs and requires 496 semantic dispatch cases per WARP adapter. These
checks cannot pass through an unsupported-adapter skip. Texture-copy checks
verify same-submission GPU input, partial readback with guard bytes,
cross-list ordering and same-texture mip copies. The optional depth-bounds
feature records either all 14 measured cases or `unsupported` with zero
cases; a capability-query error is a failure.

It saves a `checks.json` report and individual process logs under a unique
directory in the system temporary folder. `-OutputRoot D:\victory-runs`
selects another external directory. Omitting `-RequireGpu` permits the
existing optional GPU tests to skip on machines without their prerequisites;
that result does not satisfy the strict GPU gate. Independent checks keep
running after a failure, and each records its own status and error. The
final status remains failed if any required check failed.

CI adds `-NativeShaderDebug` to the strict command on `windows-2025` for pull requests and
pushes to `main`, with read-only repository permissions. It uploads only the
asset-free check logs and metadata. Existing console-SDK differential tests
and ignored tests requiring reference game data are **not** exercised by
this workflow. It does not launch a title or establish physical GPU support.
An independent `ubuntu-24.04` job runs all platform-independent workspace
tests, checks all targets/features, tests the diagnostics harness and checks
the asset policy. Windows-only tests remain the responsibility of the
Windows job; Linux success is not a Windows runtime result.

### Capture the intermittent native shader crash

The post-merge Windows run exposed an access violation in the concurrent
shader suite even after isolated checks passed. To collect its actual native
stack, install the Windows SDK Debugging Tools and run:

```powershell
pwsh -NoProfile -File scripts/Test-Victory.ps1 -RequireGpu -NativeShaderDebug
```

`Test-NativeShader.ps1` builds the exact shader test executable and runs it
three times under x64 CDB with four test threads. Second-chance access
violations capture the exception, registers, thread stacks and loaded-module
names. After any concurrent failure, separate child processes also run the
exact LDS result test and the seven existing VALU tests, preserving all
failures and per-case LDS traces. The normal concurrent workspace test
still runs afterward. A missing
debugger, timeout, native exception or incomplete Rust result fails the gate.
CI preserves `.native.log` and `native-shader.json` alongside the other
synthetic logs; no process memory dumps are written. `-CdbPath`, `-Attempts`
and `-TestThreads` are available when invoking the script directly.

The same flag also runs `Test-NativeGpu.ps1`. It launches the compressed
texture-copy and depth-bounds controls in separate CDB processes, with the
D3D12 debug layer and flushed operation traces. It captures both access
violations and native aborts, and records each result in `native-gpu.json`.
The depth control prints every original case and, on a mismatch, compares
title state updates with direct native calls and ordinary depth tests.
These extra probes do not change the required results. Independent checks
continue after a failure so their evidence is retained; any failed check
still fails the overall command.

### Compare software-renderer versions

`Test-WarpComparison.ps1` replays the two native reports against the Windows
inbox WARP and Microsoft's pinned
[WARP 1.0.22 package](https://www.nuget.org/packages/Microsoft.Direct3D.WARP/1.0.22).
Run the native checks first, then supply their exact report paths:

```powershell
pwsh -NoProfile -File scripts/Test-WarpComparison.ps1 `
  -RepositoryRoot (Get-Location).Path `
  -ShaderReportPath 'D:\VIctory-reports\native-shader.json' `
  -GpuReportPath 'D:\VIctory-reports\native-gpu.json'
```

The comparison verifies the package and x64 DLL hashes, copies the same
test executable bytes into separate external application directories, and
preserves the original test arguments and concurrency. Each child gets a
fresh cache. The debugger must confirm the expected loaded DLL path and
version before a result is accepted. `warp-comparison.json` records both
sets of results; a passing newer-runtime control cannot erase an inbox
failure. The existing target directory and installed Windows files are
unchanged. The package is for testing and development; its binaries and
symbols are excluded from the repository and CI artifacts.

CI currently runs this comparison after the normal Windows gate, retaining
failures in both. This is a diagnostic comparison, not a claim that either
runtime passes or that a physical GPU/title has been tested.

### Optional independent DXIL validation

The synthetic image-atomic matrix can also validate all 88 emitted
containers with the official DXC `dxv` executable on Windows or Linux.
Provide a locally installed, version-pinned DXC distribution, record its
version and executable hash with your results, and run:

```powershell
$env:VICTORY_DXV = 'D:\tools\dxc\bin\x64\dxv.exe'
cargo test --locked -p victory-shader --all-features `
  emit::atomic_tests::image_atomics_translate_in_both_widths_and_return_modes `
  -- --exact --nocapture
cargo test --locked -p victory-shader --all-features `
  emit::lds_tests::lds_subword_programs_translate_and_validate `
  -- --exact --nocapture
Remove-Item Env:VICTORY_DXV
```

The independent shader-hash comparison is explicitly ignored by ordinary
test discovery and can be required with an official compiler:

```powershell
$env:VICTORY_DXC = 'D:\tools\dxc\bin\x64\dxc.exe'
cargo test --locked -p victory-shader --all-features `
  emit::container::tests::shader_hash_matches_official_dxc `
  -- --ignored --exact --nocapture
Remove-Item Env:VICTORY_DXC
```

On Linux, set `VICTORY_DXV` to the absolute path of the Linux `dxv` binary
for the same Cargo command. The test fails if the supplied executable cannot
run or rejects a container. Without it, Linux still exercises synthetic
compilation and feature declarations; it does not perform independent DXIL
validation. CI does not download an unpinned DXC release. This opt-in command
runs directly because the diagnostics runner clears inherited `VICTORY_*`
variables to keep its own tests reproducible.

### Formatting baseline

The inherited source tree does not pass `cargo fmt --all --check`; its debt
includes untouched generated-style XMA tables and several runtime modules.
The initial rendering change deliberately avoids a repository-wide
format-only rewrite. The enforced gate is:

```powershell
pwsh -NoProfile -File scripts/Test-ChangedRustFormat.ps1 -BaseRef origin/main
```

It requires every **complete changed Rust file**, including new untracked
files and staged/unstaged edits, to pass the pinned formatter. Module children
are checked separately if they changed. A changed formatter configuration
requires the full-workspace check. A missing base is an error; a change with
no Rust files prints that no formatting was exercised rather than claiming
the whole repository is clean.

CI checks against the pull request's base SHA or the push's previous SHA,
with full history fetched. A manual workflow run takes `format_base_ref`
(default `HEAD^`). Local checks use `origin/main`; both `Test-Victory.ps1`
and `Invoke-VictoryDiagnostics.ps1` accept `-FormatBaseRef <base-commit>`
for another comparison. This
is a scoped gate with explicit baseline debt, not a replacement assertion
that the entire repository already satisfies rustfmt. A later formatting
cleanup can restore the full-workspace gate.

## Collect shader failures

Supply your own shader input files or directories, outside the repository:

```powershell
pwsh -NoProfile -File scripts/Invoke-VictoryDiagnostics.ps1 `
  -ShaderPath 'D:\victory-data\shader-inputs' `
  -OutputRoot 'D:\victory-runs'
```

This runs the synthetic gate, builds the `diag` executable, then runs
`translate --validate --pipelines --report ... --fail-on-error`. The resulting
`shader-report.json` groups failure causes and records each shader's
runtime-compatible hash, stage and check results. A nonzero translation
exit remains recorded even when a report is successfully collected. The
runner does not substitute that exit with a successful coverage claim.

Use the failure groups to choose an instruction or data-flow problem, then
join affected shader hashes to runtime logs. Fix compute producers feeding
indirect arguments and visible scene passes before increasing the total
translation count indiscriminately. Validate fixes with synthetic programs
and check that previously working programs retain their results.

## Reproduce the first world frame

The runner launches no title unless `-GameExe` is provided. Each invocation
creates a unique report directory; each matrix case gets independent cache
and save directories. It uses WARP unless `-Hardware` is explicitly supplied.

```powershell
pwsh -NoProfile -File scripts/Invoke-VictoryDiagnostics.ps1 `
  -GameExe 'D:\victory-data\game\title.exe' `
  -OutputRoot 'D:\victory-runs' `
  -DurationSeconds 180 `
  -GpuValidation
```

Replace `title.exe` with the actual executable. Runtime logs include debug
messages, DRED diagnostics and the host debug layer; `-GpuValidation` also
enables GPU-based validation. Inspect the log to confirm that the requested
adapter and diagnostics were actually available.

The report records the source commit, whether the working tree was dirty,
Rust and PowerShell versions, OS, installed video-controller names and
driver versions, the built executable's SHA-256, title executable SHA-256,
requested environment, process exit code, elapsed time and timeout status.
The controller inventory does not prove which adapter was selected or which
D3D12 features it supports; use the runtime's adapter/capability log for that.

`-DurationSeconds` is a finite observation window (1–3,600 seconds). At the
deadline the runner terminates the title process tree and records a timeout.
A timeout is not a rendering success or a crash diagnosis. It may interrupt
a save in that case's isolated data directory. A nonzero exit before the
deadline or a capture error stops the matrix so a failure can be inspected
before repeating it. A relaunched title retaining output pipes can produce
an incomplete capture; the process report marks this explicitly.

For repeatable navigation, use an identical initial state and input schedule.
`-SeedDataDir 'D:\victory-data\known-save-snapshot'` copies that external
snapshot into each case's data directory without modifying the snapshot.
Old `crashes.log` files are omitted. Without a seed, every case begins with
fresh saves/settings and may need first-boot navigation.
`-Keys '30:Enter,35:X'` passes the existing scripted-input switch; tailor its
times and keys to your actual scenario. The runner does not assume that
these example inputs reach the world.

## Isolate slow loops and blocked passes

Run a bounded loop-limit matrix with the same seed and input sequence:

```powershell
pwsh -NoProfile -File scripts/Invoke-VictoryDiagnostics.ps1 `
  -GameExe 'D:\victory-data\game\title.exe' `
  -SeedDataDir 'D:\victory-data\known-save-snapshot' `
  -LoopLimit 0,1024,4096 `
  -ConsoleMode series-x,series-s `
  -GpuTiming -DurationSeconds 180
```

`0` means the compiler's unchanged default loop limit; other values request
that specific diagnostic limit. A large speed change is evidence to inspect
the affected loops, descriptors and upstream buffer writes. A lower limit
can discard valid work, so it is not itself a performance fix. Every case
starts with a cold, independent cache; do not compare first-run compilation
time to a warm hardware gameplay run.

Use a real 16-digit shader hash from logs or the shader report to isolate a
pipeline, optionally collecting an otherwise identical baseline:

```powershell
pwsh -NoProfile -File scripts/Invoke-VictoryDiagnostics.ps1 `
  -GameExe 'D:\victory-data\game\title.exe' `
  -SkipPipeline '0123456789abcdef' -CompareWithoutSkips `
  -GpuTiming -DurationSeconds 180
```

The hash above is illustrative. Skipping a pass may remove the symptom by
removing necessary work. Trace its inputs and outputs before concluding
that the skipped shader is the cause. `-CaptureFrames 128,256 -DumpTargets`
enables selected frame and render-target captures in that case's external
directory. Frame numbers must be chosen from a real run, and screenshots
must be inspected manually; the runner does not infer visible geometry.

`-SkipChecks` bypasses the repeated synthetic gate after it has already been
run for the same source change. The diagnostics executable is still rebuilt
and hashed. The report records that checks were omitted.

## Hardware verification and acceptance

Once WARP logs identify the failing work, repeat the bounded scenario with
`-Hardware` on a test PC. The current README warns that world rendering can
reset a physical GPU. Do not treat WARP behavior or absence of validation
errors as proof that hardware execution is safe or correct.

For every matrix case, fill in its `observations.json` with the actual stage
reached, recognizable-scene result, character control and whether a driver
reset occurred. Track these separate acceptance gates:

| Gate | Evidence needed |
| --- | --- |
| Source/build regression | Strict synthetic CI passes for the tested commit |
| Shader correction | A meaningful synthetic result test plus a successful runtime check for affected hashes |
| First world scene | Repeatable visible geometry and correct render-target flow after the intro title card |
| GPU stability | Repeated runs without a driver reset, with adapter/driver and DRED results recorded |
| Interaction | Character input changes the visible scene and the same save can be reloaded |
| Performance | A specified scene, cache state and uninstrumented hardware build with measured frame times |
| Compatibility | Independent results per hardware/driver, wave capability and Series X/S mode |

No timeframe or FPS target is implied by passing an earlier gate. Render
queries, depth bounds, metadata planes, geometry stages, wave-size semantics
and ray-tracing builders need their own evidence when a captured dependency
shows they block the chosen scene.

All logs, caches, snapshots, captures and game-derived reports remain
outside the source tree, following [the asset policy](asset-policy.md).
The diagnostics runner uploads nothing. Share concise failure metadata and
synthetic reproductions for reviews; do not commit game-derived artifacts.

## Implementation references

- [Rust toolchain overrides](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)
- [GitHub workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [Windows 2025 runner image](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)
- [.NET argument-list escaping](https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.processstartinfo.argumentlist)
- [.NET process termination and descendant limitations](https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.process.kill)
