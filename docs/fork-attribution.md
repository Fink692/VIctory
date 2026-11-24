# Attribution for selected fork improvements

This records the source commits used when integrating selected improvements from
[fxceless/victory-fork](https://github.com/fxceless/victory-fork), reviewed at
[`4e17478`](https://github.com/fxceless/victory-fork/commit/4e17478b9c27bb530643e5b0e16a767cf4419e08),
into [Fink692/VIctory](https://github.com/Fink692/VIctory), starting from
[`03da5ec`](https://github.com/Fink692/VIctory/commit/03da5ec62ae2333aade54388ba72a4d808a5f6d9).
Each row identifies the original commit, its author and title, and the scope
adapted to this tree. Some commits in the fork are inherited upstream work;
the named original contributors retain that credit.

[VIctory by Marvisak and its contributors](https://github.com/Marvisak/VIctory)
is the common base: its loader, runtime, GPU implementation, shader translator,
format decoders and history remain the foundation of this project. The project
retains **GPL-3.0-or-later**, its existing notices and [LICENSE](../LICENSE).
Original author names below follow source commit metadata, including tool or
local account names when those are what the commits record.

## Shared source commit

The following broad commit supplied several selected changes. Its one row covers
the runtime, scheduler and format adaptations together.

| Source commit | Original author | Original title | Scope adopted here |
| --- | --- | --- | --- |
| [578d459](https://github.com/fxceless/victory-fork/commit/578d459c344aa44753e9a61ea6ba10a8c68f6ea6) | fxceless; co-author Claude Fable 5.2 | victory-gta6: this fork's changes on top of VIctory main (ffe310e) | Release the original and prepared executable buffers after mapping; adapt the shared pipeline wait reservation and priority queue; adopt selected BCPack dictionary layout and texture copy loop changes. |

The scope above excludes the console instruction emulator and the broad commit's
other experimental rendering systems. The source link identifies provenance
for the listed changes, rather than a complete merge of that commit.

## Runtime, configuration and loader

| Source commit | Original author | Original title | Scope adopted here |
| --- | --- | --- | --- |
| [257d8bb](https://github.com/fxceless/victory-fork/commit/257d8bbab241c096625ccca7ff24b8a551e3ab36) | Manuel Pérez | Audio: reduce the title's XMA read advance before adding it | Reduce the XMA ring advance before addition; retain the overflow regression. |
| [27b530e](https://github.com/fxceless/victory-fork/commit/27b530e7e14bcca992c8c128efedaf718723e6a0) | Duff | Files: send rewritten narrow paths to the host, not to our wide wrappers | Send translated narrow paths directly to host wide APIs; retain the rewrite-once regression. |
| [02fcf72](https://github.com/fxceless/victory-fork/commit/02fcf7240740e670a50bb5fcaa37e7b3525f6682) | Maraco91 | xmem: mapping or unmapping no pages changes nothing | Leave views unchanged for zero-length mappings and unmaps. |
| [36d6a79](https://github.com/fxceless/victory-fork/commit/36d6a79595ff9c24930f7c881bd34d7fd56e425a) | Maraco91 | XTaskQueue: terminating a queue ends a dispatch waiting on it | Wake manual dispatchers on queue termination, including a shared-port wakeup test. |
| [3e39eac](https://github.com/fxceless/victory-fork/commit/3e39eac1c5dc5a66ac0c98464acd18b06bcc598c) | quaternion | fix(hle): return XAsync payloads for nonnegative results | Return payload sizes and payloads for all nonnegative completion results. |
| [c88b7ac](https://github.com/fxceless/victory-fork/commit/c88b7ac1cce4d2d3b12d761bd66632f65a5a6e23) | Maraco91 | Game saves: an empty configuration id is the default, not every configuration's folder | Resolve an empty save configuration to the default directory; preserve other configurations. |
| [ff8694a](https://github.com/fxceless/victory-fork/commit/ff8694a69a3444bbbbdc37668181708a87d5b5cc) | itsRevela | Window: keep Alt and F10 from freezing the title in menu mode | Suppress keyboard menu activation through Alt/F10 and preserve other system commands. |
| [d46b078](https://github.com/fxceless/victory-fork/commit/d46b07803c80c6f05ae1738254ec98c7d50529a9) | Duff | XUser: a gamertag without a suffix has an empty modern suffix | Return an empty modern suffix for a gamertag without one. |
| [28c1b07](https://github.com/fxceless/victory-fork/commit/28c1b0735ce4230303e75c366dbd019934c0e1ce) | marquezini | XGameSave: a read result into a small buffer is E_GS_PROVIDED_BUFFER_TOO_SMALL | Map a small save-read result buffer to the save API error while allowing a retry. |
| [f0ee35e](https://github.com/fxceless/victory-fork/commit/f0ee35e9dfc700faa528ee48d3d31dd0ef101315) | marquezini | XGameSave: a full host disk is E_GS_OUT_OF_LOCAL_STORAGE, not the quota | Distinguish the save quota from host disk exhaustion; account for replacement space. |
| [376b3cf](https://github.com/fxceless/victory-fork/commit/376b3cf1785380cc397fdf89027f46e3472a6b55) | marquezini | Storage: a path buffer too small fails with E_FAIL, as on Xbox | Return the console storage API error for an undersized path buffer. |
| [cc63aa2](https://github.com/fxceless/victory-fork/commit/cc63aa269f0750b8848813c467676d3a23f188a4) | Enzo Souza Duarte | Config: ignore an empty VICTORY_TEST_TITLES | Treat an empty test-title environment override as absent. |
| [9a8908c](https://github.com/fxceless/victory-fork/commit/9a8908c3786206a669d5fbc3bbdbe2d534354fe2) | Advait Palve | core: show data_dir in config and check its asset policy | Display the data directory and include it in configuration policy checks. |
| [babe2db](https://github.com/fxceless/victory-fork/commit/babe2db61fa1125746e448cbd314c69dfec770bb) | Duff | Crash reports: show the bytes of an instruction the CPU refused | Report refused instruction bytes without reading past their page. |
| [3390e47](https://github.com/fxceless/victory-fork/commit/3390e47328e057badbb3cf2a2f565a3a05291960) | Duff | Crash reports: say what an access violation's address holds | Report the mapping and protection of a faulting memory address. |
| [b5c6678](https://github.com/fxceless/victory-fork/commit/b5c66787dafdb16e3090af49cd17682fd2713d5a) | Abdellah K. | loader: unwind a 16-byte aligned copy of the context | Keep unwind and captured contexts at their required 16-byte alignment. |
| [5f29f3a](https://github.com/fxceless/victory-fork/commit/5f29f3ae5a92eef900c77cfa0dc824cc4b812e09) | Duff | Loader: a title DLL that fails to prepare is named in the error | Include the title DLL path in preparation errors and retain the authored bad-DLL fixture. |
| [5f670bb](https://github.com/fxceless/victory-fork/commit/5f670bbfdba199a7537124adaa7ef5b0871e352b) | Duff | Loader: a trapped unimplemented import goes into the crash log | Write trapped imports to the crash log and retain the child-process regression. |
| [764fa19](https://github.com/fxceless/victory-fork/commit/764fa19373e0e03cad31804a13290656465bcacc) | lameloball2813-ui | Loader tests: build the probe fixture under CARGO_TARGET_TMPDIR | Build the authored loader probe under the configured target scratch directory. |

## Shader translation and fixtures

| Source commit | Original author | Original title | Scope adopted here |
| --- | --- | --- | --- |
| [79fb722](https://github.com/fxceless/victory-fork/commit/79fb722a530056ce1f193b2a4476901de414a8d7) | Duff | Shaders: s_ttracedata_imm is SOPP opcode 0x28, not 0x26 | Correct the trace-marker opcode and its decoder regression. |
| [f4438b3](https://github.com/fxceless/victory-fork/commit/f4438b38f435bcd0434bb2352128b7482fb223a9) | local | Shaders: reads of uninitialized SGPRs read as zero, as VGPRs do | Lower otherwise uninitialized scalar-register inputs consistently to zero. |
| [3dd467c](https://github.com/fxceless/victory-fork/commit/3dd467ccee8d6612fa41aa17537c0b182477ceac) | KoLolX3 | Shaders: a phi that only another read phi consumes is live too | Preserve live values reached through chains of shader phi nodes. |
| [24480a7](https://github.com/fxceless/victory-fork/commit/24480a73f5bba85d560aab1393c93bdc1d203e68) | TG0000 | Shaders: find the written instructions with a worklist, not repeated passes | Use a worklist for instruction liveness instead of repeated whole-program passes. |
| [51f1627](https://github.com/fxceless/victory-fork/commit/51f1627d88dd2d1cc742f95a586ef1fb7ee41b27) | Lyrnc | Shaders: v_cmp_lg is ordered on AMD hosts too (olt \| ogt, not fcmp one) | Implement ordered less-or-greater comparisons using ordered comparisons. |
| [a0efd2d](https://github.com/fxceless/victory-fork/commit/a0efd2d37709f9e7c3774b64a3a06ab616a643ba) | AlanQiang0418 | Shaders: decode GFX10 packed inline constants by operand type | Decode packed inline constants according to their operand type. |
| [2af66ff](https://github.com/fxceless/victory-fork/commit/2af66ff1d8dbd7ccd7d87488a57e2caf37d065a1) | Luiz Neves | Shaders: v_cvt_u16_f16 and v_cvt_i16_f16 saturate at 16 bits | Saturate half-float conversions to the destination 16-bit integer range. |
| [ded334b](https://github.com/fxceless/victory-fork/commit/ded334bb9172865a9be7acdb8a2047358a30e4db) | Luiz Neves | Shaders: lower v_cvt_norm_i16_f16 and v_cvt_norm_u16_f16 | Add normalized signed and unsigned 16-bit conversions from half floats. |
| [bf84134](https://github.com/fxceless/victory-fork/commit/bf84134b2be663608ad29abeae2fb05705c8fb39) | Duff | Shaders: maximums and minimums order -0 below +0, as RDNA2 does | Preserve the ordering of negative and positive zero in minimum and maximum operations. |
| [1823079](https://github.com/fxceless/victory-fork/commit/1823079f6c9ce760774146334d97282c6a9752af) | lordon1a | Shaders: lower v_cmp_class_f16 and v_cmpx_class_f16 | Add half-float class comparisons, including EXEC-updating variants. |
| [3c86053](https://github.com/fxceless/victory-fork/commit/3c860538e18fe6e1bb7b0fdcdf6daefa83904d35) | tedin7 | Shaders: lower s_bitcmp0/1_b32 and s_bitcmp0/1_b64 | Add 32-bit and 64-bit scalar bit comparisons. |
| [d5704aa](https://github.com/fxceless/victory-fork/commit/d5704aaac67201c0b3c47512b955a83526bb54bd) | Pedro Fortunato | fix(shader): lower dynamic v_perm_b32 without INT64 | Lower dynamic byte permutations without requiring 64-bit integer operations. |
| [260611a](https://github.com/fxceless/victory-fork/commit/260611acaaf5ea33748e4d74986aee500e254353) | Saitken91 | Shaders: image_sample_o with constant offsets | Support constant offsets for image sampling. |
| [c2f60d6](https://github.com/fxceless/victory-fork/commit/c2f60d6bddc228c5be5feb6e08daccbf6c4e7f94) | Duff | Shaders: a pixel shader writing SV_StencilRef declares StencilRef | Declare stencil-reference output metadata when a pixel shader writes it. |
| [404d7fd](https://github.com/fxceless/victory-fork/commit/404d7fd9bd9ee4f1d1a54eca9e8fed369ae29f83) | Duff | Shaders: PSV0 says a pixel shader runs per sample when an input does | Declare per-sample pixel execution in pipeline validation metadata. |
| [25b0f88](https://github.com/fxceless/victory-fork/commit/25b0f888cab67fc3653c7e17257075ff4c8e932a) | Dathinel | Test probe: compare direct and joined reads of a shader timestamp | Retain the authored direct-versus-joined shader timestamp probe. |
| [a3d3bc9](https://github.com/fxceless/victory-fork/commit/a3d3bc91e7c645414b09a6db910154720f499a80) | Claude | Shaders: trace descriptors chosen by s_cselect and s_cmov | Trace descriptors selected by scalar conditional operations. |
| [ffa6635](https://github.com/fxceless/victory-fork/commit/ffa663571fd0a14b9beb0889d6c48468010601fb) | Claude | Shaders: lower waterfall loops over per-lane descriptors in one pass | Integrate per-lane descriptor sources and waterfall lowering with this tree's emitter. |
| [6285e19](https://github.com/fxceless/victory-fork/commit/6285e19a8a4a36ae9d93e12a9f43b24b37a939c5) | Claude | Shaders: take the first lane's descriptor as each lane's own only in a waterfall loop | Require a waterfall proof before replacing a first-lane descriptor with per-lane values. |
| [6638098](https://github.com/fxceless/victory-fork/commit/6638098ceec122fd8ac19bbf141835d3b9a149cc) | r4. | Shaders: per-lane descriptors chosen by control flow | Trace descriptors selected by per-lane control flow and preserve reachable phi inputs. |
| [b1c8306](https://github.com/fxceless/victory-fork/commit/b1c83069db62ff3513ab1b5209df42842991cfd0) | fuzeofficial | Shaders: prove EXEC selection before collapsing a waterfall | Prove EXEC selection and retry masks before collapsing descriptor loops. |
| [6c63dea](https://github.com/fxceless/victory-fork/commit/6c63deaa89a28286b1248ed9cdd9f49bf49e32e8) | r4. | Shaders: name why a loop is no waterfall, and test the shapes found in review | Keep rejection diagnostics and authored regression shapes for invalid waterfall loops. |
| [74a9d33](https://github.com/fxceless/victory-fork/commit/74a9d3316de69b65eb64b2fa5c06089d8979ed6f) | r4. | Shader tests: the waterfall tests take the GPU one at a time, with the VALU tests | Serialize waterfall GPU fixtures with the native VALU fixtures. |
| [ffe310e](https://github.com/fxceless/victory-fork/commit/ffe310eb2098e52169f4c44db5ffdabf43a72622) | Marvis | Shaders: the console's waterfall loop over a first-lane table offset | Handle the compiler form that loads a descriptor at a first-lane table offset. |

## GPU behavior and cache publication

| Source commit | Original author | Original title | Scope adopted here |
| --- | --- | --- | --- |
| [d279e69](https://github.com/fxceless/victory-fork/commit/d279e69ae999aedfd3d463b0c45c8fd125c1ad7b) | ZazaJr24 | Make host pipeline waits configurable and prewarm pending requests (#1) | Configure the shared wait budget, remember pending requests, and promote requested pipelines. |
| [717f322](https://github.com/fxceless/victory-fork/commit/717f322c3ced51ea6f4baae4790a83efa2b2e184) | Maraco91; co-author Claude Fable 5.2 | GPU: GetDevice and the DXGI device hand out the device at every level | Use the device's interface recognition for child GetDevice and DXGI queries. |
| [31ce30f](https://github.com/fxceless/victory-fork/commit/31ce30f16954ffdf22758c6f24757a0d8ae79c62) | fuzeofficial | GPU: unbind vertex buffer slots for null view arrays | Unbind vertex-buffer slots when a null view array is supplied. |
| [7d73e2b](https://github.com/fxceless/victory-fork/commit/7d73e2b249c975d0a6ffd1be0256b8ea8db63c4e) | Codex | pipeline: remove staging files when cache publication fails | Clean staging files after a failed cache publication. |
| [0f369d6](https://github.com/fxceless/victory-fork/commit/0f369d660c9e5ace8482f2f9cc5572c9805e9416) | marquezini | Capture: R16G16B16A16_UNORM targets are integers, format 89 is 10:10:10:2 | Adapt capture format dispatch, with local corrections for signed formats and XR bias. |
| [63df363](https://github.com/fxceless/victory-fork/commit/63df36315956f1fef1dd7068a336ce5177b0baa3) | Duff | GPU: a half-float rounding carry raises the exponent, not halves the value | Carry half-float mantissa rounding into the exponent in the CPU oracle and GPU expansion. |
| [eb30a0d](https://github.com/fxceless/victory-fork/commit/eb30a0d9f4f8aee0a287563406adc9de06377cfb) | Enzo Souza Duarte | GPU: write half subnormals from f32 rectangle outputs | Support half-float subnormal rectangle outputs; adapt rounding to nearest-even. |
| [3c81420](https://github.com/fxceless/victory-fork/commit/3c814208f1160a1c70f02b13617d652927fbda88) | hophaver; co-author Claude Opus 5.5 | GPU: drop draws whose fresh tables cannot be made; 2^18 dynamic slots | Drop work when descriptor tables cannot be allocated; retain this tree's pool size and lifetime model. |

## Format decoding and documentation

| Source commit | Original author | Original title | Scope adopted here |
| --- | --- | --- | --- |
| [3e982f7](https://github.com/fxceless/victory-fork/commit/3e982f7ec436003226c134315f993d2bf23b9286) | TG0000 | BCPack: find a slot's bucket from a window table | Use a window table for BCPack bucket lookup and retain exact decoding regression checks. |
| [0f0c872](https://github.com/fxceless/victory-fork/commit/0f0c872a8f55b356c166c9c6d4185a6bda578e88) | Louis Faust | Docs: drop the private intra-doc link to MICRO in texture.rs | Remove the private rustdoc link from the texture-format documentation. |

## Adaptations and verification added in this integration

The integration in Fink692/VIctory adds or extends authored fixtures and safeguards
around the selected changes:

- Pipeline wait reservations account for concurrent recording threads and reject
  stale-frame accounting; tests cover shared allowance, early refunds and queue
  promotion without duplicate claims.
- Descriptor allocation failure propagates through draw, dispatch and indirect
  paths while retaining this tree's GPU-ordered constant-buffer uploads. A native
  fixture checks that exhausted tables do not issue work with stale bindings.
- Half-float fixes apply to the existing GPU rectangle expansion as well as its
  CPU reference. Native format cases include subnormal and rounding boundaries,
  while retaining the prior signed-zero checks.
- Capture tests check XR bias and signed formats in addition to the original
  UNORM16 correction.
- Shader ports are integrated with the existing bounded LDS accesses,
  DXIL-compatible atomic subtraction and shader cache versioning. Authored
  native instruction and waterfall fixtures are connected to strict coverage
  checks and the pinned WARP validation workflow.
- BCPack adaptations are checked against the existing decoding behavior, and the
  authored texture-copy fixtures cover layout and padding behavior.
- Runtime regression fixtures are used against the original implementation and
  the adapted implementation to distinguish observed fixes from source review.
  Windows-specific fixtures remain part of Windows execution checks.

These adaptations and tests are local integration work, distinct from the
original changes credited above. Test outcomes and performance comparisons are
recorded separately; this attribution list establishes neither complete feature
parity with the other fork nor title playability.
