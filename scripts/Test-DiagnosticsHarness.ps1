#Requires -Version 7.2
# Synthetic checks of the harness itself; no Rust toolchain, GPU or game needed.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
$temp = New-VictoryReportDirectory ([IO.Path]::GetTempPath()) 'victory-harness-test'
$pwsh = (Get-Process -Id $PID).Path
$oldInherited = [Environment]::GetEnvironmentVariable('VICTORY_DIAG_TEST_INHERITED')

function Assert-That { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
function Assert-Rejected { param([scriptblock]$Action, [string]$Message) $rejected = $false; try { & $Action | Out-Null } catch { $rejected = $true }; Assert-That $rejected $Message }

try {
    Assert-Rejected { Assert-VictoryExternalPath (Join-Path $script:VictoryRepoRoot 'logs') } 'Must reject output inside the repository.'
    Assert-Rejected { Assert-VictoryExternalPath (Join-Path $script:VictoryRepoRoot 'scripts/../new/logs') } 'Must normalize parent traversal.'
    $copy = Join-Path $temp 'other-copy'
    [void][IO.Directory]::CreateDirectory($copy)
    '' | Set-Content -LiteralPath (Join-Path $copy '.victory-root')
    Assert-Rejected { Assert-VictoryExternalPath (Join-Path $copy 'logs') } 'Must reject another marked source tree.'
    $link = Join-Path $temp 'linked-copy'
    if ($IsWindows) { [void](New-Item -ItemType Junction -Path $link -Target $copy) }
    else { [void](New-Item -ItemType SymbolicLink -Path $link -Target $copy) }
    Assert-Rejected { Assert-VictoryExternalPath (Join-Path $link 'logs') } 'Must reject paths through a junction or symlink.'
    Remove-Item -LiteralPath $link -Force

    $probe = Join-Path $temp 'probe.ps1'
    @'
param([string]$Value, [int]$ExitCode = 0, [int]$SleepSeconds = 0, [switch]$Volume)
if ($SleepSeconds) { Start-Sleep -Seconds $SleepSeconds }
if ($Volume) {
    for ($i = 0; $i -lt 4096; $i++) {
        [Console]::Out.WriteLine(('o' * 256))
        [Console]::Error.WriteLine(('e' * 256))
    }
}
[Console]::Out.WriteLine((@{ value = $Value; inherited = $env:VICTORY_DIAG_TEST_INHERITED; explicit = $env:VICTORY_DIAG_TEST_EXPLICIT } | ConvertTo-Json -Compress))
[Console]::Error.WriteLine('probe stderr')
exit $ExitCode
'@ | Set-Content -LiteralPath $probe -Encoding utf8
    [Environment]::SetEnvironmentVariable('VICTORY_DIAG_TEST_INHERITED', 'must-not-leak')
    $value = 'spaces "quotes" $dollar & ; literal $(text)'
    $result = Invoke-VictoryProcess -Executable $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $probe, '-Value', $value) -WorkingDirectory $temp -LogPrefix (Join-Path $temp 'arguments') -Environment @{ VICTORY_DIAG_TEST_EXPLICIT = 'yes' } -TimeoutSeconds 15
    Assert-VictoryProcessSucceeded $result 'argument probe'
    $got = Get-Content -LiteralPath $result.stdout_log -Raw | ConvertFrom-Json
    Assert-That ($got.value -ceq $value) 'Arguments must round-trip without shell interpretation.'
    Assert-That (-not $got.inherited) 'Inherited diagnostic variables must be removed.'
    Assert-That ($got.explicit -ceq 'yes') 'Explicit environment must reach the child.'
    Assert-That ((Get-Content -LiteralPath $result.stderr_log -Raw).Contains('probe stderr')) 'stderr must be captured.'
    Assert-Rejected { Invoke-VictoryProcess -Executable $pwsh -WorkingDirectory $temp -LogPrefix (Join-Path $temp 'arguments') } 'Must refuse to overwrite a previous log.'

    $result = Invoke-VictoryProcess -Executable $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $probe, '-ExitCode', '7', '-Volume') -WorkingDirectory $temp -LogPrefix (Join-Path $temp 'volume') -TimeoutSeconds 15
    Assert-That ($result.exit_code -eq 7 -and $result.output_complete -and -not $result.error) 'Must preserve a nonzero exit while draining both full output pipes.'
    Assert-That ((Get-Item -LiteralPath $result.stdout_log).Length -gt 1000000) 'stdout was truncated.'
    Assert-That ((Get-Item -LiteralPath $result.stderr_log).Length -gt 1000000) 'stderr was truncated.'

    $result = Invoke-VictoryProcess -Executable $pwsh -Arguments @('-NoProfile', '-NonInteractive', '-File', $probe, '-SleepSeconds', '30') -WorkingDirectory $temp -LogPrefix (Join-Path $temp 'timeout') -TimeoutSeconds 1
    Assert-That ($result.timed_out -and $result.duration_ms -lt 15000) 'A long-running child must be stopped at the deadline.'
    Assert-That (Test-Path -LiteralPath (Join-Path $temp 'timeout.process.json')) 'Timeout metadata must be written.'

    $coverage = Get-VictoryAtomic64Coverage "test diagnostics`nVICTORY_ATOMIC64_RESULT=executed adapters=2 cases=704`r`n"
    Assert-That ($coverage.status -eq 'executed' -and $coverage.adapters -eq 2 -and $coverage.cases -eq 704) 'Executed semantic cases must be counted separately from test discovery.'
    $coverage = Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=unsupported adapters=0 cases=0'
    Assert-That ($coverage.status -eq 'unsupported' -and $coverage.cases -eq 0) 'Unsupported atomic64 hardware must not be reported as semantic execution.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'test result: ok. 1 passed;' } 'A passing test alone must not claim semantic execution.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=executed adapters=0 cases=0' } 'Executed results need actual adapters and cases.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=executed adapters=1 cases=351' } 'An incomplete atomic execution matrix must fail.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=executed adapters=1 cases=704' } 'Execution counts must agree with the number of adapters.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=unsupported adapters=1 cases=704' } 'Unsupported results cannot claim executed cases.'
    Assert-Rejected { Get-VictoryAtomic64Coverage "VICTORY_ATOMIC64_RESULT=unsupported adapters=0 cases=0`nVICTORY_ATOMIC64_RESULT=executed adapters=2 cases=704" } 'Conflicting execution markers must fail.'
    $coverage = Get-VictoryAtomicValidationCoverage 'VICTORY_ATOMIC_VALIDATION_RESULT=validated containers=88'
    Assert-That ($coverage.status -eq 'validated' -and $coverage.containers -eq 88) 'The atomic matrix must report independent validation.'
    Assert-Rejected { Get-VictoryAtomicValidationCoverage 'test result: ok. 1 passed;' } 'Compiler success cannot substitute for independent validation.'
    Assert-Rejected { Get-VictoryAtomicValidationCoverage 'VICTORY_ATOMIC_VALIDATION_RESULT=validated containers=87' } 'An incomplete validation matrix must fail.'
    $coverage = Get-VictoryLdsValidationCoverage 'VICTORY_LDS_VALIDATION_RESULT=validated containers=168'
    Assert-That ($coverage.status -eq 'validated' -and $coverage.containers -eq 168) 'The full LDS matrix needs independent validation.'
    Assert-Rejected { Get-VictoryLdsValidationCoverage 'VICTORY_LDS_VALIDATION_RESULT=validated containers=167' } 'An incomplete LDS validation matrix must fail.'
    Assert-Rejected { Get-VictoryLdsValidationCoverage 'test result: ok. 1 passed;' } 'LDS compiler success cannot substitute for independent validation.'
    $coverage = Get-VictoryShaderHashCoverage 'VICTORY_SHADER_HASH_RESULT=matched programs=2'
    Assert-That ($coverage.status -eq 'matched' -and $coverage.programs -eq 2) 'Shader hashes must match two independent compiler outputs.'
    Assert-Rejected { Get-VictoryShaderHashCoverage 'VICTORY_SHADER_HASH_RESULT=matched programs=1' } 'An incomplete compiler comparison must fail.'
    Assert-Rejected { Get-VictoryShaderHashCoverage 'test result: ok. 1 passed;' } 'Compiler comparison cannot pass silently without running.'
    $coverage = Get-VictoryLdsCoverage 'VICTORY_LDS_RESULT=executed adapters=2 cases=992'
    Assert-That ($coverage.adapters -eq 2 -and $coverage.cases -eq 992) 'All LDS cases must execute per adapter.'
    Assert-Rejected { Get-VictoryLdsCoverage 'VICTORY_LDS_RESULT=executed adapters=0 cases=0' } 'LDS execution is required, not optional.'
    Assert-Rejected { Get-VictoryLdsCoverage 'VICTORY_LDS_RESULT=executed adapters=1 cases=495' } 'Missing LDS execution cases must fail.'
    Assert-Rejected { Get-VictoryLdsCoverage 'VICTORY_LDS_RESULT=unsupported adapters=0 cases=0' } 'Unsupported LDS execution cannot pass the gate.'
    Assert-Rejected { Get-VictoryLdsCoverage "VICTORY_LDS_RESULT=executed adapters=1 cases=496`nVICTORY_LDS_RESULT=executed adapters=2 cases=992" } 'Ambiguous LDS execution markers must fail.'
    $coverage = Get-VictoryLdsDwordValidationCoverage 'VICTORY_LDS_DWORD_VALIDATION_RESULT=validated containers=184'
    Assert-That ($coverage.containers -eq 184) 'Every dword LDS container needs independent validation.'
    Assert-Rejected { Get-VictoryLdsDwordValidationCoverage 'VICTORY_LDS_DWORD_VALIDATION_RESULT=validated containers=183' } 'Partial dword LDS validation must fail.'
    Assert-Rejected { Get-VictoryLdsDwordValidationCoverage 'test result: ok. 1 passed;' } 'Test discovery does not establish dword LDS validation.'
    $coverage = Get-VictoryLdsDwordCoverage 'VICTORY_LDS_DWORD_RESULT=executed adapters=2 cases=1104'
    Assert-That ($coverage.adapters -eq 2 -and $coverage.cases -eq 1104) 'Every dword LDS case must execute per adapter.'
    Assert-Rejected { Get-VictoryLdsDwordCoverage 'VICTORY_LDS_DWORD_RESULT=executed adapters=1 cases=551' } 'Partial dword LDS execution must fail.'
    Assert-Rejected { Get-VictoryLdsDwordCoverage 'VICTORY_LDS_DWORD_RESULT=executed adapters=0 cases=0' } 'Dword LDS execution cannot pass with no adapter.'
    Assert-Rejected { Get-VictoryLdsDwordCoverage 'VICTORY_LDS_DWORD_RESULT=unsupported adapters=0 cases=0' } 'Dword LDS execution is required.'
    Assert-Rejected { Get-VictoryLdsDwordCoverage "VICTORY_LDS_DWORD_RESULT=executed adapters=1 cases=552`nVICTORY_LDS_DWORD_RESULT=executed adapters=2 cases=1104" } 'Ambiguous dword LDS markers must fail.'
    $constantOk = 'VICTORY_CONSTANT_RESULT=executed cases=24 validation_errors=0'
    $coverage = Get-VictoryConstantCoverage $constantOk 24
    Assert-That ($coverage.cases -eq 24 -and $coverage.validation_errors -eq 0) 'All constant cases need clean native validation.'
    Assert-Rejected { Get-VictoryConstantCoverage $constantOk 10 } 'Constant test coverage must match its particular matrix.'
    Assert-Rejected { Get-VictoryConstantCoverage $constantOk.Replace('cases=24', 'cases=23') 24 } 'Partial constant coverage must fail.'
    Assert-Rejected { Get-VictoryConstantCoverage $constantOk.Replace('validation_errors=0', 'validation_errors=1') 24 } 'Native constant errors must fail.'
    Assert-Rejected { Get-VictoryConstantCoverage 'test result: ok. 1 passed;' 24 } 'Test discovery cannot substitute for constant execution.'
    Assert-Rejected { Get-VictoryConstantCoverage ($constantOk + "`n" + $constantOk) 24 } 'Duplicate constant execution markers must fail.'
    $rectangleOk = 'VICTORY_RECTANGLE_RESULT=executed cases=12 validation_errors=0'
    $coverage = Get-VictoryRectangleCoverage $rectangleOk 12
    Assert-That ($coverage.cases -eq 12 -and $coverage.validation_errors -eq 0) 'Rectangle output requires all native cases and clean validation.'
    Assert-Rejected { Get-VictoryRectangleCoverage $rectangleOk 11 } 'Rectangle coverage must match the required matrix.'
    Assert-Rejected { Get-VictoryRectangleCoverage $rectangleOk.Replace('validation_errors=0', 'validation_errors=1') 12 } 'Native rectangle errors must fail.'
    Assert-Rejected { Get-VictoryRectangleCoverage 'test result: ok. 1 passed;' 12 } 'Test discovery cannot substitute for rectangle execution.'
    Assert-Rejected { Get-VictoryRectangleCoverage ($rectangleOk + "`n" + $rectangleOk) 12 } 'Duplicate rectangle markers must fail.'
    Assert-Rejected { Get-VictoryRectangleCoverage ($rectangleOk + "`nVICTORY_RECTANGLE_RESULT=unsupported cases=0 validation_errors=0") 12 } 'Conflicting rectangle markers must fail.'
    $copyOk = "VICTORY_MEMORY_COPY_RESULT=executed cases=6`nVICTORY_MEMORY_COPY_VALIDATION=checked unexpected_errors=0"
    $coverage = Get-VictoryMemoryCopyCoverage $copyOk
    Assert-That ($coverage.cases -eq 6 -and $coverage.validation -eq 'checked') 'All raw-copy cases require native validation.'
    Assert-Rejected { Get-VictoryMemoryCopyCoverage $copyOk.Replace('cases=6', 'cases=5') } 'Partial raw-copy coverage must fail.'
    Assert-Rejected { Get-VictoryMemoryCopyCoverage $copyOk.Replace('=checked', '=disabled') } 'Disabled native validation must not satisfy the raw-copy gate.'
    Assert-Rejected { Get-VictoryMemoryCopyCoverage $copyOk.Replace('unexpected_errors=0', 'unexpected_errors=1') } 'Native copy errors must fail.'
    Assert-Rejected { Get-VictoryMemoryCopyCoverage ($copyOk + "`n" + $copyOk) } 'Duplicate raw-copy markers must fail.'
    Assert-Rejected { Get-VictoryMemoryCopyCoverage ($copyOk + "`nVICTORY_MEMORY_COPY_VALIDATION=disabled unexpected_errors=0") } 'Conflicting native validation markers must fail.'
    $waterfallOk = 'VICTORY_WATERFALL_RESULT=executed programs=6 executions=12 lanes=384'
    $coverage = Get-VictoryWaterfallCoverage $waterfallOk
    Assert-That ($coverage.programs -eq 6 -and $coverage.executions -eq 12 -and $coverage.lanes -eq 384) 'Each waterfall execution must compare all lanes.'
    Assert-Rejected { Get-VictoryWaterfallCoverage $waterfallOk.Replace('programs=6', 'programs=5') } 'Every waterfall program must run.'
    Assert-Rejected { Get-VictoryWaterfallCoverage $waterfallOk.Replace('executions=12', 'executions=0') } 'A missing adapter must fail waterfall coverage.'
    Assert-Rejected { Get-VictoryWaterfallCoverage $waterfallOk.Replace('lanes=384', 'lanes=383') } 'Partial lane output must fail.'
    Assert-Rejected { Get-VictoryWaterfallCoverage 'VICTORY_WATERFALL_RESULT=executed programs=6 executions=7 lanes=224' } 'Uneven waterfall program coverage must fail.'
    Assert-Rejected { Get-VictoryWaterfallCoverage ($waterfallOk + "`n" + $waterfallOk) } 'Duplicate waterfall results must fail.'
    Assert-Rejected { Get-VictoryWaterfallCoverage ($waterfallOk + "`nVICTORY_WATERFALL_RESULT=unsupported programs=0 executions=0 lanes=0") } 'Conflicting waterfall results must fail.'
    Assert-Rejected { Get-VictoryWaterfallCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not waterfall execution.'
    $offsetOk = 'VICTORY_SAMPLE_OFFSET_RESULT=executed cases=10 pixels=160'
    $coverage = Get-VictorySampleOffsetCoverage $offsetOk
    Assert-That ($coverage.cases -eq 10 -and $coverage.pixels -eq 160) 'Each sample offset execution must compare all pixels.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage 'VICTORY_SAMPLE_OFFSET_RESULT=executed cases=4 pixels=64' } 'Every texture offset must run.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage $offsetOk.Replace('pixels=160', 'pixels=159') } 'Partial pixel output must fail.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage 'VICTORY_SAMPLE_OFFSET_RESULT=executed cases=0 pixels=0' } 'A missing adapter must fail offset coverage.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage ($offsetOk + "`n" + $offsetOk) } 'Duplicate offset results must fail.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage ($offsetOk + "`nVICTORY_SAMPLE_OFFSET_RESULT=unsupported cases=0 pixels=0") } 'Conflicting offset results must fail.'
    Assert-Rejected { Get-VictorySampleOffsetCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not texture offset execution.'
    $descriptorOk = 'VICTORY_DESCRIPTOR_EXHAUSTION_RESULT=executed cases=3 validation_errors=0'
    $coverage = Get-VictoryDescriptorExhaustionCoverage $descriptorOk
    Assert-That ($coverage.cases -eq 3 -and $coverage.validation_errors -eq 0) 'Descriptor exhaustion needs every clean drop/recovery scenario.'
    Assert-Rejected { Get-VictoryDescriptorExhaustionCoverage $descriptorOk.Replace('cases=3', 'cases=2') } 'Partial descriptor exhaustion coverage must fail.'
    Assert-Rejected { Get-VictoryDescriptorExhaustionCoverage $descriptorOk.Replace('validation_errors=0', 'validation_errors=1') } 'Descriptor errors must fail.'
    Assert-Rejected { Get-VictoryDescriptorExhaustionCoverage ($descriptorOk + "`n" + $descriptorOk) } 'Duplicate descriptor results must fail.'
    Assert-Rejected { Get-VictoryDescriptorExhaustionCoverage ($descriptorOk + "`nVICTORY_DESCRIPTOR_EXHAUSTION_RESULT=unsupported cases=0 validation_errors=0") } 'Conflicting descriptor results must fail.'
    Assert-Rejected { Get-VictoryDescriptorExhaustionCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not descriptor exhaustion execution.'
    $bufferViewOk = 'VICTORY_BUFFER_VIEW_RESULT=executed cases=26 validation_errors=0'
    $coverage = Get-VictoryBufferViewCoverage $bufferViewOk
    Assert-That ($coverage.cases -eq 26 -and $coverage.validation_errors -eq 0) 'Buffer views require every structured, raw and typed native scenario.'
    Assert-Rejected { Get-VictoryBufferViewCoverage $bufferViewOk.Replace('cases=26', 'cases=25') } 'Partial buffer-view coverage must fail.'
    Assert-Rejected { Get-VictoryBufferViewCoverage $bufferViewOk.Replace('validation_errors=0', 'validation_errors=1') } 'Buffer-view validation errors must fail.'
    Assert-Rejected { Get-VictoryBufferViewCoverage ($bufferViewOk + "`n" + $bufferViewOk) } 'Duplicate buffer-view results must fail.'
    Assert-Rejected { Get-VictoryBufferViewCoverage ($bufferViewOk + "`nVICTORY_BUFFER_VIEW_RESULT=unsupported cases=0 validation_errors=0") } 'Conflicting buffer-view results must fail.'
    Assert-Rejected { Get-VictoryBufferViewCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not buffer-view execution.'
    $indexedOk = 'VICTORY_INDEXED_BUFFER_RESULT=executed programs=20 executions=40 cases=520'
    $coverage = Get-VictoryIndexedBufferCoverage $indexedOk
    Assert-That ($coverage.programs -eq 20 -and $coverage.executions -eq 40 -and $coverage.cases -eq 520) 'Mixed buffer access requires every program and all offset/bounds cases.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage $indexedOk.Replace('programs=20', 'programs=19') } 'Every mixed-access program must execute.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage 'VICTORY_INDEXED_BUFFER_RESULT=executed programs=20 executions=21 cases=273' } 'Uneven mixed-access program coverage must fail.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage $indexedOk.Replace('cases=520', 'cases=519') } 'Partial mixed-access bounds coverage must fail.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage 'VICTORY_INDEXED_BUFFER_RESULT=executed programs=20 executions=0 cases=0' } 'A missing adapter cannot satisfy mixed-access execution.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage ($indexedOk + "`n" + $indexedOk) } 'Duplicate mixed-access results must fail.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage ($indexedOk + "`nVICTORY_INDEXED_BUFFER_RESULT=unsupported programs=0 executions=0 cases=0") } 'Conflicting mixed-access results must fail.'
    Assert-Rejected { Get-VictoryIndexedBufferCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not mixed-access execution.'
    $depthOk = 'VICTORY_DEPTH_VALIDATION_RESULT=validated containers=3'
    $coverage = Get-VictoryDepthValidationCoverage $depthOk
    Assert-That ($coverage.containers -eq 3) 'All depth export modes require independent DXIL validation.'
    Assert-Rejected { Get-VictoryDepthValidationCoverage $depthOk.Replace('containers=3', 'containers=2') } 'Partial depth validation must fail.'
    Assert-Rejected { Get-VictoryDepthValidationCoverage ($depthOk + "`n" + $depthOk) } 'Duplicate depth validation markers must fail.'
    Assert-Rejected { Get-VictoryDepthValidationCoverage ($depthOk + "`nVICTORY_DEPTH_VALIDATION_RESULT=unsupported containers=0") } 'Conflicting depth validation markers must fail.'
    Assert-Rejected { Get-VictoryDepthValidationCoverage 'test result: ok. 1 passed;' } 'Discovery alone cannot establish depth validation.'
    $packedOk = 'VICTORY_PACKED_BUFFER_RESULT=executed formats=23 backends=2 cases=252 structured_cases=72'
    $coverage = Get-VictoryPackedBufferCoverage $packedOk
    Assert-That ($coverage.formats -eq 23 -and $coverage.backends -eq 2 -and $coverage.cases -eq 252 -and $coverage.structured_cases -eq 72) 'Packed buffers require every format and all raw/structured cases.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage $packedOk.Replace('formats=23', 'formats=22') } 'Every buffer format must execute.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage $packedOk.Replace('cases=252', 'cases=251') } 'Partial packed-buffer coverage must fail.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage $packedOk.Replace('structured_cases=72', 'structured_cases=71') } 'Partial structured-format coverage must fail.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage $packedOk.Replace(' structured_cases=72', '') } 'Packed-buffer execution must prove the structured path too.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage 'VICTORY_PACKED_BUFFER_RESULT=executed formats=23 backends=0 cases=0 structured_cases=0' } 'A missing backend cannot satisfy packed-buffer execution.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage ($packedOk + "`n" + $packedOk) } 'Duplicate packed-buffer results must fail.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage ($packedOk + "`nVICTORY_PACKED_BUFFER_RESULT=unsupported formats=0 backends=0 cases=0 structured_cases=0") } 'Conflicting packed-buffer results must fail.'
    Assert-Rejected { Get-VictoryPackedBufferCoverage 'test result: ok. 1 passed;' } 'Discovery alone is not packed-buffer execution.'
    $coverage = Get-VictoryDepthBoundsCoverage 'VICTORY_DEPTH_BOUNDS_RESULT=executed cases=14'
    Assert-That ($coverage.status -eq 'executed' -and $coverage.cases -eq 14) 'Depth-bounds execution must include every case.'
    $coverage = Get-VictoryDepthBoundsCoverage 'VICTORY_DEPTH_BOUNDS_RESULT=unsupported cases=0'
    Assert-That ($coverage.status -eq 'unsupported' -and $coverage.cases -eq 0) 'Unavailable depth bounds must remain explicit.'
    Assert-Rejected { Get-VictoryDepthBoundsCoverage 'VICTORY_DEPTH_BOUNDS_RESULT=executed cases=13' } 'Partial depth-bounds execution must fail.'
    Assert-Rejected { Get-VictoryDepthBoundsCoverage 'VICTORY_DEPTH_BOUNDS_RESULT=unsupported cases=14' } 'Unsupported depth bounds cannot claim execution.'
    Assert-Rejected { Get-VictoryDepthBoundsCoverage 'test result: ok. 1 passed;' } 'A passing test alone cannot establish depth-bounds coverage.'
    $nativeOk = "VICTORY_NATIVE_DEBUGGER_READY`ntest result: ok. 79 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s`n"
    $coverage = Get-VictoryNativeShaderCoverage ($nativeOk + $nativeOk)
    Assert-That ($coverage.status -eq 'completed' -and $coverage.passed -eq 79 -and $coverage.ignored -eq 0) 'Native coverage must tolerate a duplicate console/debugger log.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage 'test result: ok. 79 passed; 0 failed; 0 ignored;' } 'A debugger run needs its setup marker.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage 'VICTORY_NATIVE_DEBUGGER_READY' } 'Debugger success without a completed test suite must fail.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage ($nativeOk + "VICTORY_NATIVE_UNHANDLED`n") } 'An unhandled native exception must fail even with a zero debugger exit and earlier passing summary.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage ($nativeOk + "test result: FAILED. 1 passed; 1 failed; 0 ignored;`n") } 'A failed Rust suite must fail the debugger gate.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage ($nativeOk + "test result: ok. 78 passed; 0 failed; 0 ignored;`n") } 'Conflicting native terminal results must fail.'
    Assert-Rejected { Get-VictoryNativeShaderCoverage "VICTORY_NATIVE_DEBUGGER_READY`ntest result: ok. 0 passed; 0 failed; 0 ignored;`n" } 'A debugger run must discover actual tests.'
    $warpPath = 'C:\victory runtime\d3d10warp.dll'
    $warpVersion = '1.0.22.0'
    $warpModule = "VICTORY_WARP_MODULE_BEGIN`r`n    Image path: $warpPath`r`n    File version: $warpVersion`r`nVICTORY_WARP_MODULE_END`r`n"
    $evidence = Get-VictoryWarpModuleEvidence -Output ($warpModule + $warpModule) -ExpectedPath $warpPath.ToUpperInvariant() -ExpectedVersion $warpVersion
    Assert-That ($evidence.path -ceq $warpPath -and $evidence.file_version -eq $warpVersion) 'Loaded WARP evidence must tolerate duplicate output and case-insensitive Windows paths.'
    $evidence = Get-VictoryWarpModuleEvidence -Output ($warpModule + "VICTORY_NATIVE_UNHANDLED`n")
    Assert-That ($evidence.path -ceq $warpPath -and $evidence.file_version -eq $warpVersion) 'A failed native process must retain its observed runtime identity independently of test success.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $nativeOk -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'Passing tests cannot substitute for loaded WARP module evidence.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output '' -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'Missing runtime output must not establish a WARP identity.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $warpModule.Replace("    Image path: $warpPath`r`n", '') -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'Module evidence must include the loaded DLL path.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $warpModule.Replace("    File version: $warpVersion`r`n", '') -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'Module evidence must include the loaded file version.'
    $otherPath = $warpModule.Replace($warpPath, 'C:\Windows\System32\d3d10warp.dll')
    $otherVersion = $warpModule.Replace($warpVersion, '10.0.26100.33648')
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $otherPath -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'The wrong loaded WARP path must fail even with the expected version.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $otherVersion -ExpectedPath $warpPath -ExpectedVersion $warpVersion } 'The wrong loaded WARP version must fail even with the expected path.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output ($warpModule + $otherPath) } 'Conflicting loaded module paths must fail even without a pinned expectation.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output ($warpModule + $otherVersion) } 'Conflicting loaded module versions must fail even without a pinned expectation.'
    Assert-Rejected { Get-VictoryWarpModuleEvidence -Output $warpModule -ExpectedPath $warpPath } 'A runtime requirement must specify both path and version.'
    Write-Host 'Diagnostics harness checks passed: external paths, argument quoting, environment isolation, dual-stream capture, nonzero exit, timeout, no overwrite, explicit semantic coverage, native debugger failure propagation and loaded WARP identity.'
} finally {
    [Environment]::SetEnvironmentVariable('VICTORY_DIAG_TEST_INHERITED', $oldInherited)
    if (Test-Path -LiteralPath (Join-Path $temp 'linked-copy')) { Remove-Item -LiteralPath (Join-Path $temp 'linked-copy') -Force }
    Remove-Item -LiteralPath $temp -Recurse -Force
}
