#Requires -Version 7.2
<#
.SYNOPSIS
Build and test VIctory on Windows without game files or a console SDK.
.PARAMETER RequireGpu
Require the synthetic DXIL validation and WARP pipeline tests to run,
failing instead of counting a missing validator/adapter as a passing test.
#>
[CmdletBinding()]
param(
    [string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) 'victory-checks'),
    [string]$FormatBaseRef = 'origin/main',
    [switch]$RequireGpu
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Test-Victory.ps1 requires Windows x64 and the MSVC build tools.' }
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
$report = New-VictoryReportDirectory $OutputRoot 'checks'
$config = Join-Path $report 'victory.toml'
'[paths]' | Set-Content -LiteralPath $config -Encoding utf8
$environment = @{
    VICTORY_CONFIG = $config
    VICTORY_CACHE_DIR = (Join-Path $report 'cache')
    VICTORY_DATA_DIR = (Join-Path $report 'data')
    VICTORY_WARP = '1'
    VICTORY_D3D12_WARP = '1'
    CARGO_TARGET_DIR = (Join-Path $script:VictoryRepoRoot 'target')
    CARGO_TERM_COLOR = 'never'
}
$checks = [Collections.Generic.List[object]]::new()
$summary = [ordered]@{
    schema_version = 1
    started_at_utc = [DateTime]::UtcNow.ToString('o')
    status = 'running'
    require_gpu = [bool]$RequireGpu
    formatting_base = $FormatBaseRef
    formatting_scope = 'Complete changed Rust files; full workspace when rustfmt configuration changes.'
    checks = $checks
    limitations = @('No game files are executed.', 'Console-SDK differential tests and ignored reference-data tests are not part of this gate.', 'WARP does not establish physical GPU compatibility or world rendering.')
}

function Invoke-Check {
    param([string]$Name, [string]$Executable, [string[]]$Arguments, [switch]$MustRunOneTest)
    Write-Host "Checking $Name..."
    $result = Invoke-VictoryProcess -Executable $Executable -Arguments $Arguments -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report $Name) -Environment $environment -TimeoutSeconds 1200
    $checks.Add([ordered]@{ name = $Name; process = $result })
    Assert-VictoryProcessSucceeded $result $Name
    if ($MustRunOneTest) {
        $output = (Get-Content -LiteralPath $result.stdout_log -Raw) + (Get-Content -LiteralPath $result.stderr_log -Raw)
        if ($output -notmatch 'test result: ok\. 1 passed;' -or $output -match '; skipped') {
            throw "$Name did not run its required test successfully. See $($result.stdout_log)."
        }
    }
}

try {
    Invoke-Check 'toolchain' 'rustc' @('--version', '--verbose')
    Invoke-Check 'format' (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File', (Join-Path $PSScriptRoot 'Test-ChangedRustFormat.ps1'), '-BaseRef', $FormatBaseRef)
    if ($RequireGpu) {
        $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
        $validator = Get-ChildItem -LiteralPath $kits -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            Sort-Object { [version]$_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64/dxil.dll' } |
            Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
            Select-Object -First 1
        if (-not $validator) { throw 'A Windows SDK with x64/dxil.dll is required for the GPU gate.' }
        $environment.VICTORY_DXIL = $validator
        $summary['dxil_validator'] = $validator
        Invoke-Check 'dxil-validator' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::tests::constant_color_validates', '--', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'warp-pipeline' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::tests::stub_pipeline_is_created', '--', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'warp-query-results' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'query::tests::timestamp_queries_execute_and_preserve_destination_guards', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'warp-empty-queries' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'query::tests::empty_occlusion_and_pipeline_queries_return_measured_zeroes', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'warp-query-copies' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'query::tests::resolved_queries_feed_gpu_copies_across_lists', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'warp-indirect-budget' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'indirect::tests::indirect_budget_cannot_wrap_and_ignores_inactive_records', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest
    }
    Invoke-Check 'workspace-tests' 'cargo' @('test', '--locked', '--workspace', '--all-features', '--', '--nocapture')
    Invoke-Check 'clippy' 'cargo' @('clippy', '--locked', '--workspace', '--all-targets', '--all-features')
    Invoke-Check 'release-build' 'cargo' @('build', '--locked', '--release', '-p', 'victory')
    Invoke-Check 'diagnostics-build' 'cargo' @('build', '--locked', '--profile', 'diag', '-p', 'victory', '--features', 'diagnostics')
    Invoke-Check 'asset-policy' 'cargo' @('run', '--locked', '--quiet', '-p', 'xtask', '--', 'policy')
    $summary.status = 'passed'
} catch {
    $summary.status = 'failed'
    $summary['error'] = $_.Exception.Message
    throw
} finally {
    $summary['finished_at_utc'] = [DateTime]::UtcNow.ToString('o')
    Write-VictoryJson (Join-Path $report 'checks.json') $summary
    Write-Host "Validation report: $report"
}
