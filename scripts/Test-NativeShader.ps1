#Requires -Version 7.2
<#
.SYNOPSIS
Run the synthetic shader suite under the Windows SDK debugger.
.DESCRIPTION
Captures native exception context and module/stack names without writing
process dumps. Tests remain concurrent, and a debugger exit code of zero
does not override a missing or failed Rust test result.
#>
[CmdletBinding()]
param(
    [string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) 'victory-native-shader'),
    [string]$CdbPath,
    [ValidateRange(1, 5)][int]$Attempts = 3,
    [ValidateRange(2, 32)][int]$TestThreads = 4,
    [string]$ExpectedWarpDll,
    [string]$ExpectedWarpVersion
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Native shader debugging requires Windows x64.' }
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
$report = New-VictoryReportDirectory $OutputRoot 'native-shader'
$config = Join-Path $report 'victory.toml'
'[paths]' | Set-Content -LiteralPath $config -Encoding utf8
$environment = @{
    VICTORY_CONFIG = $config
    VICTORY_CACHE_DIR = (Join-Path $report 'cache')
    VICTORY_DATA_DIR = (Join-Path $report 'data')
    VICTORY_WARP = '1'
    VICTORY_D3D12_WARP = '1'
    VICTORY_ATOMIC_TRACE = '1'
    VICTORY_LDS_TRACE = '1'
    CARGO_TARGET_DIR = (Join-Path $script:VictoryRepoRoot 'target')
    CARGO_TERM_COLOR = 'never'
}
if ($env:VICTORY_DXIL) { $environment.VICTORY_DXIL = $env:VICTORY_DXIL }
$runs = [Collections.Generic.List[object]]::new()
$failures = [Collections.Generic.List[string]]::new()
$summary = [ordered]@{
    schema_version = 1
    started_at_utc = [DateTime]::UtcNow.ToString('o')
    status = 'running'
    test_threads = $TestThreads
    attempts = $Attempts
    runs = $runs
    limitations = @('Synthetic shaders only; optional SDK and adapter checks retain their existing coverage limits.', 'No process dumps, heap contents or environment dumps are collected.')
}

try {
    if ([bool]$ExpectedWarpDll -ne [bool]$ExpectedWarpVersion) {
        throw 'Provide both -ExpectedWarpDll and -ExpectedWarpVersion.'
    }
    if ($ExpectedWarpDll) {
        $ExpectedWarpDll = [IO.Path]::GetFullPath($ExpectedWarpDll)
        $summary['expected_warp'] = [ordered]@{ path = $ExpectedWarpDll; file_version = $ExpectedWarpVersion }
    }
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
    $build = Invoke-VictoryProcess -Executable 'cargo' -Arguments @('test', '--locked', '-p', 'victory-shader', '--all-features', '--lib', '--no-run', '--message-format=json') -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report 'build') -Environment $environment -TimeoutSeconds 1200
    $summary['build'] = $build
    Assert-VictoryProcessSucceeded $build 'native-shader-build'
    $executables = @(Get-Content -LiteralPath $build.stdout_log | ForEach-Object {
        if ($_.StartsWith('{')) {
            $message = $_ | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.target.name -eq 'victory_shader' -and $message.executable) {
                $message.executable
            }
        }
    } | Select-Object -Unique)
    if ($executables.Count -ne 1 -or -not (Test-Path -LiteralPath $executables[0] -PathType Leaf)) {
        throw 'Cargo did not identify exactly one native victory-shader test executable.'
    }
    $executable = $executables[0]
    $summary['test_executable'] = $executable
    $commands = Join-Path $report 'debugger-commands.txt'
    # Only second-chance access violations terminate this diagnostic run.
    # Normal first-chance exception handling is unchanged. The explicit marker
    # is a failure even when CDB itself subsequently exits successfully.
    # A live exception already selects its faulting context; .ecxr is for dumps.
    @'
sxd -c2 ".echo VICTORY_NATIVE_UNHANDLED; .lastevent; .exr -1; r; kn 64; ~* kn 32; lm; lmvm d3d10warp; lmvm D3D12Core; q" av
sxe -c ".echo VICTORY_WARP_MODULE_BEGIN; lmvm d3d10warp; .echo VICTORY_WARP_MODULE_END; g" ld:*d3d10warp*
.echo VICTORY_NATIVE_DEBUGGER_READY
g
'@ | Set-Content -LiteralPath $commands -Encoding ascii
    $symbols = (Split-Path $executable -Parent) + ';srv*' + (Join-Path $report 'symbols') + '*https://msdl.microsoft.com/download/symbols'
    function Invoke-NativeShaderRun {
        param(
            [Parameter(Mandatory)][string]$Label,
            [string[]]$TestArguments = @(),
            [uint32]$ExpectedTests = 0,
            [switch]$RequireLdsCoverage
        )
        $prefix = Join-Path $report $Label
        $nativeLog = $prefix + '.native.log'
        $arguments = @('-G', '-lines', '-y', $symbols, '-logo', $nativeLog, '-cf', $commands, $executable) + $TestArguments + @('--nocapture', "--test-threads=$TestThreads")
        $runEnvironment = $environment.Clone()
        $runEnvironment.VICTORY_CACHE_DIR = Join-Path $report "$Label-cache"
        $runEnvironment.VICTORY_DATA_DIR = Join-Path $report "$Label-data"
        $run = [ordered]@{ label = $Label; status = 'running'; native_log = $nativeLog }
        $runs.Add($run)
        try {
            $result = Invoke-VictoryProcess -Executable $CdbPath -Arguments $arguments -WorkingDirectory $script:VictoryRepoRoot -LogPrefix $prefix -Environment $runEnvironment -TimeoutSeconds 300
            $run['process'] = $result
            $output = (Get-Content -LiteralPath $result.stdout_log -Raw) + (Get-Content -LiteralPath $result.stderr_log -Raw)
            if (Test-Path -LiteralPath $nativeLog) { $output += Get-Content -LiteralPath $nativeLog -Raw }
            if ($output) {
                $run['loaded_warp'] = Get-VictoryWarpModuleEvidence -Output $output -ExpectedPath $ExpectedWarpDll -ExpectedVersion $ExpectedWarpVersion
            }
            Assert-VictoryProcessSucceeded $result "native-shader-$Label"
            $run['coverage'] = Get-VictoryNativeShaderCoverage $output
            if ($ExpectedTests -and ($run.coverage.passed -ne $ExpectedTests -or $run.coverage.ignored -ne 0)) {
                throw "$Label must complete exactly $ExpectedTests tests with none ignored."
            }
            if ($RequireLdsCoverage) {
                $markers = @([regex]::Matches($output, '(?m)^VICTORY_LDS_RESULT=[^\r\n]*') | ForEach-Object { $_.Value } | Select-Object -Unique)
                $run['semantic_coverage'] = Get-VictoryLdsCoverage ($markers -join "`n")
            }
            $run.status = 'passed'
            Write-Host "Native shader $Label completed: $($run.coverage.passed) tests reported passed."
        } catch {
            $run.status = 'failed'
            $run['error'] = $_.Exception.Message
            $failures.Add($Label)
            Write-Host "Native shader $Label failed: $($run.error)"
        }
    }
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        Invoke-NativeShaderRun -Label "attempt-$attempt"
    }
    if ($failures.Count) {
        # Independent child processes distinguish the newly added LDS shader
        # from the older concurrent VALU cases. Their results cannot erase a
        # failed full-suite run, and neither control serializes the suite.
        Invoke-NativeShaderRun -Label 'isolated-lds' -TestArguments @('emit::lds_tests::lds_subword_results_preserve_adjacent_bytes', '--exact') -ExpectedTests 1 -RequireLdsCoverage
        Invoke-NativeShaderRun -Label 'isolated-valu' -TestArguments @('emit::valu_tests::') -ExpectedTests 13
        throw "Native shader runs failed: $($failures -join ', '). See each run's logs."
    }
    $summary.status = 'passed'
} catch {
    $summary.status = 'failed'
    $summary['error'] = $_.Exception.Message
    throw
} finally {
    $summary['finished_at_utc'] = [DateTime]::UtcNow.ToString('o')
    Write-VictoryJson (Join-Path $report 'native-shader.json') $summary
    Write-Host "Native shader report: $report"
}
