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

    $coverage = Get-VictoryAtomic64Coverage "test diagnostics`nVICTORY_ATOMIC64_RESULT=executed adapters=1 cases=704`r`n"
    Assert-That ($coverage.status -eq 'executed' -and $coverage.adapters -eq 1 -and $coverage.cases -eq 704) 'Executed semantic cases must be counted separately from test discovery.'
    $coverage = Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=unsupported adapters=0 cases=0'
    Assert-That ($coverage.status -eq 'unsupported' -and $coverage.cases -eq 0) 'Unsupported atomic64 hardware must not be reported as semantic execution.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'test result: ok. 1 passed;' } 'A passing test alone must not claim semantic execution.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=executed adapters=0 cases=0' } 'Executed results need actual adapters and cases.'
    Assert-Rejected { Get-VictoryAtomic64Coverage 'VICTORY_ATOMIC64_RESULT=unsupported adapters=1 cases=704' } 'Unsupported results cannot claim executed cases.'
    Assert-Rejected { Get-VictoryAtomic64Coverage "VICTORY_ATOMIC64_RESULT=unsupported adapters=0 cases=0`nVICTORY_ATOMIC64_RESULT=executed adapters=1 cases=704" } 'Conflicting execution markers must fail.'
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
    Write-Host 'Diagnostics harness checks passed: external paths, argument quoting, environment isolation, dual-stream capture, nonzero exit, timeout, no overwrite, explicit semantic coverage and native debugger failure propagation.'
} finally {
    [Environment]::SetEnvironmentVariable('VICTORY_DIAG_TEST_INHERITED', $oldInherited)
    if (Test-Path -LiteralPath (Join-Path $temp 'linked-copy')) { Remove-Item -LiteralPath (Join-Path $temp 'linked-copy') -Force }
    Remove-Item -LiteralPath $temp -Recurse -Force
}
