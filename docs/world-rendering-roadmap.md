# World rendering: implementation and acceptance

The next product milestone is a recognizable first world scene, with usable
input and repeatable loading, without a device reset. The repository's
historical menu and loading results do not establish that milestone. This
change supplies compiler/runtime fixes and a way to collect the missing
evidence; it does not claim that the title is playable.

## Work implemented in this branch

| Area | Change | Evidence and remaining check |
| --- | --- | --- |
| Integer image atomics | Lower 11 operations at 32 and 64 bits, including compare/exchange, signed extrema, returned old values and inactive lanes. Declare the required DXIL/SFI0 features. | Synthetic cases cover width, return behavior and execution guards; all 88 emitted containers passed the official DXC validator. The Windows gate records semantic execution separately from an unsupported adapter. Inspect the tested commit's CI result; no physical-GPU/title execution is claimed. |
| Resource handles and lane masks | Key cached handles by both resource-property words; declare wave operations when emitting lane-index queries. | Regression tests and independent validation cover the float/integer handle collision and missing feature flag. Atomic resource annotations also follow the canonical output of Microsoft's HLSL compiler. |
| Shader identity | Replace the all-zero `HASH` part with standard MD5 of each program's bitcode, independently of the DXBC container signature; invalidate old generated shader cache entries. | Padding/reference vectors, deterministic/different-program regressions and two authored programs compiled with official DXC verify the payload. The hosted Windows reference comparison passed, but the concurrent native crash persisted after this correction. |
| Capability checks and shader cache | Check actual host int64, typed-atomic, descriptor-heap-atomic and fixed-wave capabilities, including requirements read from cached shader containers. Refuse unsupported wave sizes before pipeline creation and invalidate old generated shader cache entries. | Windows code cross-compiles. Boundary tests cover wave-size support and failed feature queries. Confirm actual adapter features and behavior from a Windows run. |
| GPU queries and ordering | Allocate host query heaps, record begin/end/resolve operations, preserve query lifetime through submission, track host buffer states and perform buffer copies on the GPU. Use the host queue's clock/frequency. | Required hosted Windows/WARP tests passed for timestamp destinations, measured empty occlusion/statistics queries and resolve-to-copy ordering. Physical GPU/title execution remains required. |
| Texture copies and readback | Copy buffer footprints through GPU staging so earlier GPU writes are visible; implement texture-to-buffer readback, partial destinations and same-texture mip copies. Validate ranges and repack title row pitches/offsets into native layouts; expand validated BC edges to full physical blocks. | All five authored Windows copy tests passed, including GPU-produced input, guard bytes, partial pixels, reverse-recorded producer/consumer lists, compressed small mips and selected depth/stencil planes. The BC regression also passed on both compared WARP versions. Title verification remains required. |
| GPU-produced constants and raw copies | Stage unaligned root CBVs and table CBVs from their GPU shadows at each use; reuse stable descriptors while refreshing their contents in command order. Route the console raw-address copy command through GPU buffer copies. | Authored native fixtures check root/table shader reads, repeated draws and submissions, reverse-recorded lists, overlap, padding and invalid ranges. The strict Windows gate requires these tests; title verification remains separate. |
| Byte/halfword shared memory | Lower 14 LDS subword instructions, including signed reads and d16 half-register preservation; use atomic word updates/reads so neighboring byte writers do not race. | All 168 synthetic containers passed independent DXV validation. The mandatory Windows test executed all 496 dispatch cases per adapter, including cross-wave neighboring fields, on both compared WARP versions. Physical GPU/title verification remains required. |
| Single-dword shared-memory bounds | Guard 32-bit LDS reads, writes and supported atomics before forming a memory pointer; out-of-range reads return zero, writes are dropped and inactive destinations remain unchanged. Lower subtraction as atomic addition of the wrapping negated operand and invalidate older generated shader caches. | Independent DXIL validation and required native result tests cover valid edges, out-of-range addresses, inactive lanes and atomic returned values. Multi-dword boundary/alignment modes remain outside this change. |
| CPU fence waits and submission errors | Implement blocking null-event fence waits with completion retained across later fence rewinds. Stop on native signal/event/wait failure before copying GPU data back to the CPU. | Threaded tests cover registration races, independent targets and rewinds. Normal native tests exercise successful submissions; injected native synchronization failures have not been executed. |
| Depth bounds | Preserve the enable bit in graphics/mesh pipeline streams, check host support, route dynamic title state and reset native bounds. | All 14 measured cases passed on WARP 1.0.22 with valid descriptor bindings and no unexpected debug-layer errors. Inbox WARP 10.0.26100.33648 advertised support but failed the depth-disabled combination. Unsupported capabilities remain explicit; physical GPU/title verification is separate. |
| Indirect execution | Saturate the work budget, honor actual count-buffer values, validate argument/count ranges and transition the count buffer for indirect reads. | CPU boundary tests and the required hosted WARP regression passed for overflow, inactive records and binding conflicts. Physical GPU/title execution remains required. |
| Shader census | Add deterministic JSON failure groups and per-program runtime-compatible hashes; distinguish emitted, validated and pipeline-created results. Surface scan and check-initialization errors. | CLI tests cover failure gates, unavailable checks, hashes, report serialization and refusing output replacement. Full title inputs are not available in this workspace. |
| Reproducible diagnostics | Add isolated saves/caches, deadlines, separate process logs, loop-limit/pipeline/console experiments and explicit observation records. | Synthetic process tests passed on Linux and hosted Windows for quoting, environment isolation, output capture, failures, deadlines and output paths. They do not execute the title. |
| Automated verification | Add Windows synthetic DXIL/WARP gates and portable tests, build/lint checks and asset-policy checks. | Local native tests, Windows cross-compilation and hosted Linux checks passed. Check the complete Windows job and recorded coverage for the exact commit before accepting it. |

The actual title's shader totals, frame times, screenshots and compatibility
claims must be remeasured. A source or compiler test must not be reported as
a hardware result. Some Windows-only tests use their own linked PE executable;
those remain Windows tests, while synthetic parser/compiler tests run on
Linux as well.

The first native single-dword LDS matrix exposed an existing emitter error:
its first subtraction dispatch suspended WARP in all three concurrent runs.
The containers had passed validation, but Microsoft's
[DXIL specification](https://github.com/microsoft/DirectXShaderCompiler/blob/main/docs/DXIL.rst)
forbids `atomicrmw sub` for groupshared memory. Subtraction now uses one atomic
add with a wrapping negated operand, matching an authored official-DXC
reference and retaining all 552 semantic cases. See
[the initial failing run](https://github.com/Fink692/VIctory/actions/runs/45469493365)
and the current PR workflow for the native result after that correction.

The first hosted Windows run confirmed the required query/indirect checks
but exposed an access violation in the larger shader suite. That finding
led to correcting the atomic resource annotation and adding isolated
validation/execution checks with explicit coverage markers. The next run
validated all 88 containers and executed the first 16 unguarded cases, then
rejected the first fixed-wave guarded pipeline. Guarded and unguarded
execution now have separate 352-case checks; required wave sizes are checked
against measured device capabilities, and pipeline failures include native
debug messages when available. A passing subset is not a passing workflow;
use the current result linked from the pull request, including any subsequent
fixes and capability-based coverage gaps.

The first PR's Windows gate subsequently passed, but the merged `main` run
exposed another access violation in the concurrent shader suite. Neither
the annotation correction nor the later shader HASH correction eliminated
that failure on inbox WARP. A controlled comparison then ran identical
executables and arguments against inbox `10.0.26100.33648` and official WARP
`1.0.22.0`. The inbox runtime failed all three concurrent attempts and the
isolated VALU control; the pinned runtime passed all three attempts (83
tests each), all seven VALU tests, 496 LDS cases and all 14 depth cases.
The loaded paths/versions and executable hashes were checked, and the full
shader/VALU runs used the same SDK validator. See
[the comparison run](https://github.com/Fink692/VIctory/actions/runs/45429956002).

The complete Windows gate now prepares that pinned software runtime and
requires its actual loaded identity in the native controls. The concurrent
suite still runs three times with four test threads; a zero debugger exit
cannot substitute for a complete passing Rust result. The optional manual
inbox comparison preserves failures. Acceptance still requires the complete
current Windows job, including workspace tests and builds, to pass; the
comparison alone did not run the entire workspace on the newer runtime.

The buffer-state work covers known vertex/index, descriptor/root-buffer,
indirect, query and buffer/texture-copy paths. It does not establish arbitrary
bindless-buffer correctness or coherence between separate shadows of
overlapping title memory. CPU-expanded geometry still needs separate work
when its input is produced by the GPU. Unaligned root and table constants now
use GPU-ordered staging. A known simultaneous read/UAV binding of one host buffer is
rejected explicitly; implementing legal aliasing for those passes remains
open. Cross-queue `Wait` dependencies are also still unimplemented; the new
CPU fence wait does not provide asynchronous queue scheduling.

## Remaining work, in dependency order

Use a fixed save, navigation sequence, executable build and observation
window for each comparison. Start with the WARP workflow in
[diagnostics.md](diagnostics.md); join failure-report hashes to the shaders
actually used by the missing world passes.

| Work | Next concrete step | Completion evidence |
| --- | --- | --- |
| First visible scene and GPU hangs | Capture the work immediately after the intro title card; identify its producers, consumers and failing GPU commands. | Repeatable visible geometry and a useful DRED/debug-layer result for any failure; no reset on repeated hardware runs. |
| Remaining shader instructions/data flow | Rank failing runtime hashes, then reduce each required failure to a synthetic instruction/resource case. | Meaningful result tests, valid DXIL, and the affected pass executing correctly. A larger offline census alone is insufficient. |
| Merged geometry stages | Determine the stage contracts, inter-stage storage and output topology of required programs before implementing lowering. | Synthetic topology/output comparisons and visible geometry in the dependent scene. |
| Ray-tracing builder shaders | Identify required builder kernels and their buffer/descriptor layouts from runtime evidence. | Valid structures and correct consuming passes, with bounded execution on a supported adapter. |
| Wave-size requirements | Check subgroup assumptions, barriers and cross-lane operations against the actual host capabilities. | Result comparisons for supported wave sizes; explicit rejection when the required semantics cannot be provided. |
| Render-target metadata | Trace metadata/layout transitions for the missing scene's attachments. | Synthetic clear/copy/readback checks and correct pixels in the consuming pass. |
| Depth bounds | Execute the new state mapping and boundary tests on supported hardware, then inspect a required title pass. | All 14 synthetic depth-boundary cases and a captured pass using the state; unsupported capability remains explicit. |
| HDR and scaling | Establish the required formats, transfer functions, viewport and presentation contracts. | SDR/HDR and window-size comparisons with correct color and dimensions. |
| Performance and frame pacing | First establish correct rendering; then separate shader compilation, CPU memory copies, queue waits and GPU work in a fixed scene. | Frame-time distributions from an uninstrumented hardware build, with cache state and configuration recorded. |
| Loading throughput | Measure conversion, I/O and writes into title memory independently before changing concurrency. | Repeated loading measurements with identical input/cache state and no streaming regressions. |
| Audio | Add evidence for Opus output and the currently silent reverb/speech paths. | Synthetic/reference comparisons where available and synchronized live playback. |
| Input and system UI | Verify controller input, text entry, trigger-rumble support and title relaunch on Windows. | Live interaction and restart/save continuity tests; unsupported device capabilities reported explicitly. |
| Series S mode | Repeat the exact accepted scene and save workflow in Series S mode. | Separate correctness, stability and performance results; Series X success does not imply Series S success. |

These items remain open. User-supplied title files and a Windows PC with a
physical GPU were not available to the development workspace. Hosted WARP
checks exercise synthetic inputs. The diagnostics scripts make title runs concrete and bounded;
they do not manufacture observations or substitute placeholders for missing
runtime behavior.

## Reviewing a result

Keep the source commit, executable hash, adapter/driver, requested and actual
features, cache state and console mode with every result. Fill in each run's
`observations.json` after inspecting the actual window or captures. Preserve
the difference between these outcomes:

1. A program translated.
2. Its bytecode validated.
3. A host pipeline was created.
4. A synthetic program produced the expected values.
5. The title's dependent pass produced correct pixels.
6. A repeated scene met stability and measured performance requirements.

Keep all title inputs and derived logs, reports, shaders and captures outside
the repository, as required by the [asset policy](asset-policy.md).
