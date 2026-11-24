# World rendering: implementation and acceptance

The next product milestone is a recognizable first world scene, with usable
input and repeatable loading, without a device reset. The repository's
historical menu and loading results do not establish that milestone. This
change supplies compiler/runtime fixes and a way to collect the missing
evidence; it does not claim that the title is playable.

## Work implemented in this branch

| Area | Change | Evidence and remaining check |
| --- | --- | --- |
| Integer image atomics | Lower 11 operations at 32 and 64 bits, including compare/exchange, signed extrema, returned old values and inactive lanes. Declare the required DXIL/SFI0 features. | Synthetic cases cover width, return behavior and execution guards; all 88 emitted containers passed the official DXC validator. A semantic execution test is included for capable Windows adapters; execution has not been observed here. |
| Resource handles and lane masks | Key cached handles by both resource-property words; declare wave operations when emitting lane-index queries. | Regression tests and independent validation cover the float/U64 handle collision and missing feature flag. |
| Capability checks and shader cache | Check actual host int64, typed-atomic and descriptor-heap-atomic capabilities, including shaders loaded from cache. Invalidate old generated shader cache entries. | Windows code cross-compiles. Confirm actual adapter features and behavior from a Windows run. |
| GPU queries and ordering | Allocate host query heaps, record begin/end/resolve operations, preserve query lifetime through submission, track host buffer states and perform buffer copies on the GPU. Use the host queue's clock/frequency. | Synthetic Windows tests check timestamp destinations, measured empty occlusion/statistics queries and resolve-to-copy ordering. Windows execution remains required. |
| Indirect execution | Saturate the work budget, honor actual count-buffer values, validate argument/count ranges and transition the count buffer for indirect reads. | CPU boundary tests and an opt-in WARP execution regression cover overflow and inactive records. Physical GPU/title execution remains required. |
| Shader census | Add deterministic JSON failure groups and per-program runtime-compatible hashes; distinguish emitted, validated and pipeline-created results. Surface scan and check-initialization errors. | CLI tests cover failure gates, unavailable checks, hashes, report serialization and refusing output replacement. Full title inputs are not available in this workspace. |
| Reproducible diagnostics | Add isolated saves/caches, deadlines, separate process logs, loop-limit/pipeline/console experiments and explicit observation records. | Synthetic process tests exercise quoting, environment isolation, output capture, failures, deadlines and output paths. They do not execute the title. |
| Automated verification | Add Windows synthetic DXIL/WARP gates and portable tests, build/lint checks and asset-policy checks. | Local native tests and Windows cross-compilation are available; the remote Windows job must execute for runtime evidence. |

The actual title's shader totals, frame times, screenshots and compatibility
claims must be remeasured. A source or compiler test must not be reported as
a hardware result. Some Windows-only tests use their own linked PE executable;
those remain Windows tests, while synthetic parser/compiler tests run on
Linux as well.

The buffer-state work covers known vertex/index, descriptor/root-buffer,
indirect, query and buffer-copy paths. It does not establish arbitrary
bindless-buffer correctness or coherence between separate shadows of
overlapping title memory. CPU-expanded geometry and CPU-snapshotted
constants/uploads still need separate work when their inputs are produced
by the GPU. A known simultaneous read/UAV binding of one host buffer is
rejected explicitly; implementing legal aliasing for those passes remains
open.

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
| Depth bounds | Map required title state and feature support to the host; define unsupported behavior explicitly. | A synthetic depth-boundary result test and a captured pass that uses the state. |
| HDR and scaling | Establish the required formats, transfer functions, viewport and presentation contracts. | SDR/HDR and window-size comparisons with correct color and dimensions. |
| Performance and frame pacing | First establish correct rendering; then separate shader compilation, CPU memory copies, queue waits and GPU work in a fixed scene. | Frame-time distributions from an uninstrumented hardware build, with cache state and configuration recorded. |
| Loading throughput | Measure conversion, I/O and writes into title memory independently before changing concurrency. | Repeated loading measurements with identical input/cache state and no streaming regressions. |
| Audio | Add evidence for Opus output and the currently silent reverb/speech paths. | Synthetic/reference comparisons where available and synchronized live playback. |
| Input and system UI | Verify controller input, text entry, trigger-rumble support and title relaunch on Windows. | Live interaction and restart/save continuity tests; unsupported device capabilities reported explicitly. |
| Series S mode | Repeat the exact accepted scene and save workflow in Series S mode. | Separate correctness, stability and performance results; Series X success does not imply Series S success. |

These items remain open. Several require user-supplied title files and a
Windows D3D12 machine, neither of which is available to this Linux build
environment. The diagnostics scripts make those runs concrete and bounded;
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
