# Comparison with fxceless/victory-fork

Compared on 2026-11-24 against fixed revisions:

- Our baseline: [`03da5ec`](https://github.com/Fink692/VIctory/commit/03da5ec62ae2333aade54388ba72a4d808a5f6d9).
- Other fork: [`4e17478`](https://github.com/fxceless/victory-fork/commit/4e17478b9c27bb530643e5b0e16a767cf4419e08).

The other fork contains substantially more rendering features. This change
ports the improvements that could be reviewed and checked with authored
fixtures while retaining our GPU ordering, replay, atomic, depth, texture-copy,
query and diagnostic protections. It does not merge the entire fork.

## What each version does better

| Area | Evidence and decision |
|---|---|
| Visible game world | The fork's [pinned README](https://github.com/fxceless/victory-fork/blob/4e17478b9c27bb530643e5b0e16a767cf4419e08/README.md) reports a visible prologue, incomplete lighting and about 2–4 FPS on an RTX 4070. Those are its author's observations. We did not execute a title, and cannot rank gameplay speed or correctness on equivalent hardware. |
| Shader coverage | The fork contains newer upstream opcode, numerical, metadata and per-lane descriptor fixes. Selected ports add supported operations and correctly track descriptors through branches and recognized waterfall loops. New synthetic tests cover the added behavior and unsafe loop forms are rejected. |
| CPU texture work | Its BCPack lookup and fixed-width texture-copy loops are measurably faster on the same authored inputs. Both were adopted, along with its BC7 dictionary-layout correction. |
| Pipeline compilation | Its shared wait budget and request priority address excessive waits across recording threads. We adapted these with explicit reservation/refund accounting, stale-frame protection and tests. |
| Runtime services | Seven test-only regressions demonstrated defects in our old baseline. The selected upstream fixes address save-folder isolation, async results, queue wakeup, audio arithmetic, configuration and API results; related Windows-only fixes have authored tests. |
| GPU ordering and repeat execution | We retain our GPU-produced constants, ordered raw copies, retained descriptors, rectangle expansion, fence semantics and native regression matrices. Some fork performance paths release descriptors after execution or use experimental queue lifetimes, so a wholesale replacement would lose these protections. |
| Verification and diagnostics | Our full portable workspace builds on Linux; the compared fork fails compilation there in Windows-only loader code. Its Windows CI succeeds. Our pinned WARP, strict case markers, native debugger reports and retained CI artifacts remain in place, with new required descriptor and shader gates. Test counts alone are not a correctness ranking. |

## Measured CPU changes

These measurements use Linux x86_64 and Rust 1.98.1, optimized code, the same
authored inputs for both implementations, one CPU affinity and five alternating
paired samples. Medians are shown; the shared host introduced timing variation.
The full samples and methodology are in [fork-comparison-data.json](fork-comparison-data.json).
These are component throughput measurements, not game FPS or loading-time claims.

### BCPack decompression

Each sample decodes 16,384 blocks forty times using the same synthetic tables
and streams. The fork's benchmark helper was also applied to the unchanged
baseline production code.

| Format | Old baseline, ns/block | Fork/adopted implementation, ns/block | Throughput ratio |
|---|---:|---:|---:|
| BC1 | 183.1 | 129.3 | 1.42× |
| BC4 | 185.3 | 128.2 | 1.45× |
| BC7 | 624.7 | 438.4 | 1.42× |

The port checks bucket selection against a scan for every slot and model,
checks decoding against the previous arithmetic, and round-trips all six
supported format layouts. The BC7 dictionary alignment regression fails on
the original baseline and passes with the fork's correction.

### Texture tiling and detiling

Each sample performs thirty-two copies, verifies complete output, and uses
identical generated input bytes. The partial-row case exercises a non-power-of-two
width. Existing tests additionally verify mip chains, array/volume slices and
untouched row padding.

| Elements × bytes | Detiling throughput ratio | Tiling throughput ratio |
|---|---:|---:|
| 1024 × 512 × 4 | 2.98× | 2.77× |
| 2048 × 1024 × 8 | 1.67× | 1.92× |
| 1023 × 769 × 16 | 1.36× | 1.84× |

The imported benchmark and the new authored texture benchmark can be run on
the current revision with:

```text
cargo test --release -p victory-formats bcpack_throughput -- --ignored --nocapture
cargo test --release -p victory-formats texture_copy_throughput -- --ignored --nocapture
```

For a direct historical comparison, use the same test helpers and build flags
on both pinned production sources; an older revision without those helpers
does not run a benchmark just because its test filter returns successfully.

### Shader writer worklist

The selected upstream writer change replaces repeated liveness scans with a
worklist. Three paired release samples, each timing five writes of an authored
register/phi graph, produced the same byte counts and output hashes:

| Synthetic graph size | Old median | New median |
|---|---:|---:|
| 4,417 instructions | 3.851 ms | 0.247 ms |
| 33,569 instructions | 43.264 ms | 1.763 ms |
| 132,865 instructions | 408.907 ms | 7.305 ms |

These graphs stress the writer's dependency traversal. The results measure
that stage on those graphs, not whole-game shader compilation or GPU execution.
The ignored `emit::bitcode::tests::bench_write` benchmark retains the fixture.

## Correctness evidence

Test-only additions to the original baseline reproduced seven runtime defects:
an empty test-title override, overflowing XMA read advancement, missing payloads
for successful nonzero async results, a blocked dispatcher after termination,
save migration reaching another configuration, a wrong small-buffer save error,
and an incorrect modern gamertag suffix. The corresponding tests pass after
the selected upstream ports.

Compiling the actual pure Rust half-conversion and capture functions from each
revision demonstrated a half-rounding carry error and loss of all 2,046 signed
half subnormals in our old implementation. The fork fixes both; those fixes
are adapted into our GPU rectangle implementation too. Its UNORM16 capture fix
was adopted, and remaining signed-channel and XR-bias mistakes were corrected.
The XR conversion follows [Microsoft's rule](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/xr-bias-to-float-conversion-rules).

A shader regression also demonstrates that the old transitive phi analysis
could overlook an unsupported timestamp read through two joins. The new analysis
treats the joined and direct forms consistently. Additional authored shaders
check per-lane texture/buffer selection, constant sample offsets, half operations,
NaN comparisons, signed zero and pixel-stage metadata.

Windows acceptance uses `scripts/Test-Victory.ps1 -RequireGpu -NativeShaderDebug`.
It retains every prior required gate and adds explicit coverage requirements for:

- Six waterfall programs, with all 32 lanes checked in every execution.
- Five texture offsets per backend, with all 16 pixels checked each time.
- Three descriptor exhaustion/drop/recovery scenarios with zero native errors.

The rectangle matrix remains twenty-four scenarios; its half-edge scenario now
includes every signed half subnormal and retains the existing signed-zero checks.
The final pull request and its CI artifacts record the native execution results;
cross-compilation by itself is not native acceptance.

## Limits and changes not adopted

The fork's broad shader optimizer, ray traversal/builders, optional wave64
emulation, deferred submission, relay thread, card-memory hashing and in-place
buffers need separate end-to-end validation. They were not enabled by this port.
Its geometry-stage rectangle expansion also needs indexed, indirect and nonlinear
vertex-shader coverage before replacing our tested input-expansion path. The
fork's implicit-LOD and explicit-gather shortcuts were not copied as complete
sampling implementations.

The optional console-instruction emulator was excluded: its source contains
an explicit GPL-2.0-only provenance notice despite the enclosing fork's
GPL-3.0-or-later declaration, and its memory/condition-code behavior needs
further review. This comparison does not resolve that source's licensing.

The task-queue wakeup fix is deliberately narrow. Delayed/in-flight callback
cancellation, termination-callback ordering and isolation between composite
queues remain incomplete in both implementations. A public-API reproduction
shows a delayed callback can be stranded after termination and then run uncanceled
if dispatched again. Correcting that requires queue-owned task accounting;
waking a blocked manual dispatcher does not establish complete shutdown semantics.

No title assets were imported, no game executable was run, and these changes
do not establish a playable world or physical-GPU compatibility. See
[the rendering roadmap](world-rendering-roadmap.md) for remaining work and
[the attribution record](fork-attribution.md) for the selected original commits
and their authors.
