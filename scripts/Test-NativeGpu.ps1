#Requires -Version 7.2
<#
.SYNOPSIS
Capture native diagnostics for the synthetic texture-copy and depth controls.
.DESCRIPTION
Runs each required GPU control in its own process under the Windows SDK
debugger. Captures debug-layer messages and unhandled exception stacks as
text. Any failed control fails the overall check; no process dumps are written.
#>
[CmdletBinding()]
param(
    [string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) 'victory-native-gpu'),
    [string]$CdbPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Native GPU debugging requires Windows x64.' }
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
$report = New-VictoryReportDirectory $OutputRoot 'native-gpu'
$config = Join-Path $report 'victory.toml'
'[paths]' | Set-Content -LiteralPath $config -Encoding utf8
$environment = @{
    VICTORY_CONFIG = $config
    VICTORY_CACHE_DIR = (Join-Path $report 'cache')
    VICTORY_DATA_DIR = (Join-Path $report 'data')
    VICTORY_WARP = '1'
    VICTORY_D3D12_WARP = '1'
    VICTORY_D3D12_DEBUG = '1'
    VICTORY_COPY_TRACE = '1'
    VICTORY_DEPTH_BOUNDS_TRACE = '1'
    CARGO_TARGET_DIR = (Join-Path $script:VictoryRepoRoot 'target')
    CARGO_TERM_COLOR = 'never'
}
$runs = [Collections.Generic.List[object]]::new()
$failures = [Collections.Generic.List[string]]::new()
$summary = [ordered]@{
    schema_version = 1
    started_at_utc = [DateTime]::UtcNow.ToString('o')
    status = 'running'
    runs = $runs
    limitations = @('Synthetic GPU resources only; depth bounds may be explicitly unsupported.', 'No process dumps, heap contents or environment dumps are collected.')
}

try {
    if (-not $CdbPath) {
        $command = Get-Command cdb.exe -ErrorAction SilentlyContinue
        if ($command) { $CdbPath = $command.Source }
        else { $CdbPath = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/Debuggers/x64/cdb.exe' }
    }
    if (-not (Test-Path -LiteralPath $CdbPath -PathType Leaf)) {
        throw 'Install the Windows SDK Debugging Tools (x64 cdb.exe), or pass -CdbPath.'
    }
    $CdbPath = [IO.Path]::GetFullPath($CdbPath)
    $summary['debugger'] = $CdbPath
    $build = Invoke-VictoryProcess -Executable 'cargo' -Arguments @('test', '--locked', '-p', 'victory-gpu', '--all-features', '--lib', '--no-run', '--message-format=json') -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report 'build') -Environment $environment -TimeoutSeconds 1200
    $summary['build'] = $build
    Assert-VictoryProcessSucceeded $build 'native-gpu-build'
    $executables = @(Get-Content -LiteralPath $build.stdout_log | ForEach-Object {
        if ($_.StartsWith('{')) {
            $message = $_ | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.target.name -eq 'victory_gpu' -and $message.executable) {
                $message.executable
            }
        }
    } | Select-Object -Unique)
    if ($executables.Count -ne 1 -or -not (Test-Path -LiteralPath $executables[0] -PathType Leaf)) {
        throw 'Cargo did not identify exactly one native victory-gpu test executable.'
    }
    $executable = $executables[0]
    $summary['test_executable'] = $executable
    $commands = Join-Path $report 'debugger-commands.txt'
    # Live second-chance events already select the faulting register context.
    # CDB can return zero after q; the marker and Rust result are checked below.
    $exceptionCommands = '.echo VICTORY_NATIVE_UNHANDLED; .lastevent; .exr -1; r; kn 64; ~* kn 32; lm; lmvm d3d10warp; lmvm D3D12Core; q'
    @"
sxd -c2 "$exceptionCommands" av
sxd -c2 "$exceptionCommands" 0xc0000409
.echo VICTORY_NATIVE_DEBUGGER_READY
g
"@ | Set-Content -LiteralPath $commands -Encoding ascii
    $symbols = (Split-Path $executable -Parent) + ';srv*' + (Join-Path $report 'symbols') + '*https://msdl.microsoft.com/download/symbols'
    $controls = @(
        @{ label = 'block-compressed-copy'; test = 'resource::copy_tests::block_compressed_small_mips_round_trip'; depth = $false },
        @{ label = 'depth-bounds'; test = 'pipeline::host::depth_bounds_tests::depth_bounds_filter_stored_depth_and_reset'; depth = $true }
    )
    foreach ($control in $controls) {
        $label = $control.label
        $prefix = Join-Path $report $label
        $nativeLog = $prefix + '.native.log'
        $arguments = @('-G', '-lines', '-y', $symbols, '-logo', $nativeLog, '-cf', $commands, $executable, $control.test, '--ignored', '--exact', '--nocapture')
        $runEnvironment = $environment.Clone()
        $runEnvironment.VICTORY_CACHE_DIR = Join-Path $report "$label-cache"
        $runEnvironment.VICTORY_DATA_DIR = Join-Path $report "$label-data"
        $run = [ordered]@{ label = $label; test = $control.test; status = 'running'; native_log = $nativeLog }
        $runs.Add($run)
        try {
            $result = Invoke-VictoryProcess -Executable $CdbPath -Arguments $arguments -WorkingDirectory $script:VictoryRepoRoot -LogPrefix $prefix -Environment $runEnvironment -TimeoutSeconds 300
            $run['process'] = $result
            Assert-VictoryProcessSucceeded $result "native-gpu-$label"
            $output = (Get-Content -LiteralPath $result.stdout_log -Raw) + (Get-Content -LiteralPath $result.stderr_log -Raw)
            if (Test-Path -LiteralPath $nativeLog) { $output += Get-Content -LiteralPath $nativeLog -Raw }
            $run['coverage'] = Get-VictoryNativeShaderCoverage $output
            if ($run.coverage.passed -ne 1 -or $run.coverage.ignored -ne 0) {
                throw "$label must complete exactly one test with none ignored."
            }
            if ($control.depth) {
                $markers = @([regex]::Matches($output, '(?m)^VICTORY_DEPTH_BOUNDS_RESULT=[^\r\n]*') | ForEach-Object { $_.Value } | Select-Object -Unique)
                $run['semantic_coverage'] = Get-VictoryDepthBoundsCoverage ($markers -join "`n")
            }
            $run.status = 'passed'
            Write-Host "Native GPU $label completed."
        } catch {
            $run.status = 'failed'
            $run['error'] = $_.Exception.Message
            $failures.Add($label)
            Write-Host "Native GPU $label failed: $($run.error)"
        }
    }
    if ($failures.Count) { throw "Native GPU controls failed: $($failures -join ', '). See each control's logs." }
    $summary.status = 'passed'
} catch {
    $summary.status = 'failed'
    $summary['error'] = $_.Exception.Message
    throw
} finally {
    $summary['finished_at_utc'] = [DateTime]::UtcNow.ToString('o')
    Write-VictoryJson (Join-Path $report 'native-gpu.json') $summary
    Write-Host "Native GPU report: $report"
}
