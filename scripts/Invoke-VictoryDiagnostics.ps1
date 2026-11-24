#Requires -Version 7.2
<#
.SYNOPSIS
Run bounded Windows diagnostics with independent caches and save directories.
.DESCRIPTION
Without -GameExe, only synthetic checks/builds run. A title run requires an
explicit executable outside the repository. WARP is the default adapter.
All diagnostics stay in a new directory outside the repository; nothing is
uploaded. See docs/diagnostics.md for interpretation and reproducibility.
#>
[CmdletBinding()]
param(
    [string]$GameExe,
    [string[]]$ShaderPath = @(),
    [string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) 'victory-diagnostics'),
    [string]$FormatBaseRef = 'origin/main',
    [string]$WarpPackagePath,
    [ValidateSet('series-x', 'series-s')][string[]]$ConsoleMode = @('series-x'),
    [ValidateRange(0, 1048576)][uint32[]]$LoopLimit = @(0),
    [ValidatePattern('^[0-9a-fA-F]{16}$')][string[]]$SkipPipeline = @(),
    [switch]$CompareWithoutSkips,
    [ValidateRange(1, 3600)][int]$DurationSeconds = 180,
    [string]$SeedDataDir,
    [string]$Keys,
    [switch]$Hardware,
    [switch]$GpuValidation,
    [switch]$GpuTiming,
    [ValidateCount(0, 256)][uint32[]]$CaptureFrames = @(),
    [switch]$DumpTargets,
    [switch]$SkipChecks
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'VIctory runtime diagnostics require Windows x64.' }
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
. (Join-Path $PSScriptRoot 'Initialize-VictoryWarp.ps1')
if ($DumpTargets -and $CaptureFrames.Count -eq 0) { throw '-DumpTargets requires an explicit -CaptureFrames list.' }
if ($ConsoleMode.Count -eq 0 -or $LoopLimit.Count -eq 0) { throw 'The experiment matrix must have a console mode and loop limit.' }
if ($GameExe) {
    $GameExe = Assert-VictoryExternalPath $GameExe
    if (-not (Test-Path -LiteralPath $GameExe -PathType Leaf)) { throw "Game executable not found: $GameExe" }
}
$ShaderPath = @($ShaderPath | ForEach-Object {
    $path = Assert-VictoryExternalPath $_
    if (-not (Test-Path -LiteralPath $path)) { throw "Shader input not found: $path" }
    $path
})
if ($SeedDataDir) {
    $SeedDataDir = Assert-VictoryExternalPath $SeedDataDir
    if (-not (Test-Path -LiteralPath $SeedDataDir -PathType Container)) { throw "Seed data directory not found: $SeedDataDir" }
    $destination = Assert-VictoryExternalPath $OutputRoot
    $seedPrefix = $SeedDataDir.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if ($destination.Equals($SeedDataDir, [StringComparison]::OrdinalIgnoreCase) -or $destination.StartsWith($seedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The diagnostics output directory cannot be inside the seed data directory.'
    }
}
$report = New-VictoryReportDirectory $OutputRoot 'diagnostics'
$config = Join-Path $report 'victory.toml'
'[paths]' | Set-Content -LiteralPath $config -Encoding utf8
$environment = @{
    VICTORY_CONFIG = $config
    VICTORY_CACHE_DIR = (Join-Path $report 'build-cache')
    VICTORY_DATA_DIR = (Join-Path $report 'build-data')
    CARGO_TARGET_DIR = (Join-Path $script:VictoryRepoRoot 'target')
    CARGO_TERM_COLOR = 'never'
}
$runs = [Collections.Generic.List[object]]::new()
$summary = [ordered]@{
    schema_version = 1
    started_at_utc = [DateTime]::UtcNow.ToString('o')
    status = 'running'
    adapter_requested = $(if ($Hardware) { 'hardware' } else { 'warp' })
    checks_requested = (-not [bool]$SkipChecks)
    formatting_base = $FormatBaseRef
    console_modes = @($ConsoleMode)
    loop_limits = @($LoopLimit)
    title_path = $GameExe
    seed_data_path = $SeedDataDir
    runs = $runs
    limitations = @('A process exit or timeout does not establish rendering correctness.', 'Timing with WARP, GPU validation or per-command instrumentation is not a gameplay benchmark.', 'Actual adapter capabilities must be read from runtime logs; the video-controller inventory is not a feature check.')
}

function Read-Identity {
    param([string]$Name, [string]$Executable, [string[]]$Arguments)
    $result = Invoke-VictoryProcess -Executable $Executable -Arguments $Arguments -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report $Name) -Environment $environment -TimeoutSeconds 120
    Assert-VictoryProcessSucceeded $result $Name
    return ([string](Get-Content -LiteralPath $result.stdout_log -Raw)).Trim()
}

function Copy-SeedData {
    param([string]$Destination)
    if (-not $SeedDataDir) { return }
    # Enumerate first, rejecting links before any copying. Original saves
    # remain untouched and old crash reports cannot contaminate a new run.
    $items = @(Get-ChildItem -LiteralPath $SeedDataDir -Recurse -Force)
    foreach ($item in $items) { [void](Assert-VictoryExternalPath $item.FullName) }
    foreach ($item in $items) {
        $relative = [IO.Path]::GetRelativePath($SeedDataDir, $item.FullName)
        $target = Assert-VictoryExternalPath (Join-Path $Destination $relative)
        if ($item.PSIsContainer) {
            [void][IO.Directory]::CreateDirectory($target)
        } elseif ($item.Name -ne 'crashes.log') {
            [void][IO.Directory]::CreateDirectory((Split-Path $target -Parent))
            Copy-Item -LiteralPath $item.FullName -Destination $target
        }
    }
}

try {
    $summary['commit'] = Read-Identity 'git-commit' 'git' @('rev-parse', 'HEAD')
    $summary['working_tree_dirty'] = [bool](Read-Identity 'git-status' 'git' @('status', '--porcelain'))
    $summary['rustc'] = Read-Identity 'rustc' 'rustc' @('--version', '--verbose')
    $summary['os'] = [Runtime.InteropServices.RuntimeInformation]::OSDescription
    $summary['powershell'] = $PSVersionTable.PSVersion.ToString()
    $summary['video_controllers'] = @(Get-CimInstance Win32_VideoController | Select-Object Name, PNPDeviceID, DriverVersion, DriverDate)
    $warpPackage = $null
    if (-not $SkipChecks -or (-not $Hardware -and ($GameExe -or $ShaderPath.Count -gt 0))) {
        $warpPackage = Get-VictoryWarpPackage -CacheDirectory (Join-Path $report 'warp-package') -PackagePath $WarpPackagePath
    }
    if (-not $SkipChecks) {
        & (Join-Path $PSScriptRoot 'Test-Victory.ps1') -OutputRoot (Join-Path $report 'checks') -FormatBaseRef $FormatBaseRef -RequireGpu -WarpPackagePath $warpPackage.package_path
    }
    $build = Invoke-VictoryProcess -Executable 'cargo' -Arguments @('build', '--locked', '--profile', 'diag', '-p', 'victory', '--features', 'diagnostics') -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report 'build') -Environment $environment -TimeoutSeconds 1200
    Assert-VictoryProcessSucceeded $build 'diagnostics build'
    $binary = Join-Path $script:VictoryRepoRoot 'target/diag/victory.exe'
    $summary['binary_sha256'] = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    $summary['build_profile'] = 'diag'
    $summary['build_features'] = @('diagnostics')
    if (-not $Hardware -and ($GameExe -or $ShaderPath.Count -gt 0)) {
        $application = Join-Path $report 'application'
        [void][IO.Directory]::CreateDirectory($application)
        $runtimeBinary = Join-Path $application 'victory.exe'
        Copy-Item -LiteralPath $binary -Destination $runtimeBinary
        $symbols = [IO.Path]::ChangeExtension($binary, '.pdb')
        if (Test-Path -LiteralPath $symbols) { Copy-Item -LiteralPath $symbols -Destination $application }
        if ((Get-FileHash -LiteralPath $runtimeBinary -Algorithm SHA256).Hash.ToLowerInvariant() -ne $summary.binary_sha256) { throw 'The isolated diagnostics executable differs from the built executable.' }
        $summary['warp_runtime'] = Initialize-VictoryWarp -CacheDirectory (Join-Path $report 'warp-package') -ApplicationDirectory @($application) -PackagePath $warpPackage.package_path -ReportPath (Join-Path $report 'warp-runtime.json')
        $binary = $runtimeBinary
    }
    $summary['binary_path'] = $binary
    if ($ShaderPath.Count -gt 0) {
        $scanEnvironment = $environment.Clone()
        if (-not $Hardware) { $scanEnvironment.VICTORY_WARP = '1' }
        $scanReport = Join-Path $report 'shader-report.json'
        $scanArgs = @('translate') + $ShaderPath + @('--validate', '--pipelines', '--report', $scanReport, '--fail-on-error')
        $scan = Invoke-VictoryProcess -Executable $binary -Arguments $scanArgs -WorkingDirectory $report -LogPrefix (Join-Path $report 'shader-scan') -Environment $scanEnvironment -TimeoutSeconds 3600
        $summary['shader_scan'] = $scan
        if ($scan.error -or $scan.timed_out -or -not (Test-Path -LiteralPath $scanReport)) {
            throw 'Shader scan did not complete a report. See shader-scan process and log files.'
        }
        # Unsupported programs are expected in a diagnostic census; retain
        # the nonzero exit and failure report, then allow an explicit title run.
        if ($scan.exit_code -ne 0) { Write-Host 'Shader scan found failures; inspect shader-report.json before treating any coverage count as complete.' }
    }
    if (-not $GameExe) {
        $summary.status = $(if ($ShaderPath.Count -gt 0) { 'shader-scan-collected-unverified' } elseif ($SkipChecks) { 'build-only' } else { 'synthetic-checks-only' })
        Write-Host 'No title was launched; supply -GameExe for a runtime experiment.'
    } else {
        $summary['title_sha256'] = (Get-FileHash -LiteralPath $GameExe -Algorithm SHA256).Hash.ToLowerInvariant()
        $variants = [Collections.Generic.List[object]]::new()
        if ($CompareWithoutSkips -and $SkipPipeline.Count -gt 0) { $variants.Add(@{ name = 'baseline'; skip = @() }) }
        $variants.Add(@{ name = $(if ($SkipPipeline.Count -gt 0) { 'skip' } else { 'baseline' }); skip = @($SkipPipeline) })
        $index = 0
        foreach ($mode in $ConsoleMode) {
            foreach ($limit in $LoopLimit) {
                foreach ($variant in $variants) {
                    $index++
                    $name = '{0:D2}-{1}-loop-{2}-{3}' -f $index, $mode, $limit, $variant.name
                    $runDir = Join-Path $report $name
                    [void][IO.Directory]::CreateDirectory($runDir)
                    $data = Join-Path $runDir 'data'
                    [void][IO.Directory]::CreateDirectory($data)
                    Copy-SeedData $data
                    $runEnvironment = $environment.Clone()
                    $runEnvironment.VICTORY_DATA_DIR = $data
                    $runEnvironment.VICTORY_CACHE_DIR = Join-Path $runDir 'cache'
                    $runEnvironment.VICTORY_GAME_DIR = Split-Path $GameExe -Parent
                    $runEnvironment.VICTORY_LOG = 'victory_gpu=debug,victory_shader=debug,info'
                    $runEnvironment.VICTORY_D3D12_DEBUG = '1'
                    $runEnvironment.VICTORY_D3D12_DRED = '1'
                    if (-not $Hardware) { $runEnvironment.VICTORY_D3D12_WARP = '1' }
                    if ($GpuValidation) { $runEnvironment.VICTORY_D3D12_GBV = '1' }
                    if ($GpuTiming) { $runEnvironment.VICTORY_GPU_TIMING = '1' }
                    if ($limit -gt 0) { $runEnvironment.VICTORY_SHADER_LOOP_LIMIT = $limit.ToString() }
                    if ($variant.skip.Count -gt 0) { $runEnvironment.VICTORY_SKIP_PIPELINES = $variant.skip -join ',' }
                    if ($Keys) { $runEnvironment.VICTORY_KEYS = $Keys }
                    if ($CaptureFrames.Count -gt 0) {
                        $runEnvironment.VICTORY_CAPTURE = Join-Path $runDir 'frames'
                        $runEnvironment.VICTORY_CAPTURE_FRAMES = $CaptureFrames -join ','
                        if ($DumpTargets) { $runEnvironment.VICTORY_DUMP_TARGETS = '1' }
                    }
                    Write-Host "Running $name for up to $DurationSeconds seconds; logs: $runDir"
                    Write-VictoryJson (Join-Path $runDir 'settings.json') $runEnvironment
                    $result = Invoke-VictoryProcess -Executable $binary -Arguments @('run', $GameExe, '--execute', '--console', $mode) -WorkingDirectory $runDir -LogPrefix (Join-Path $runDir 'title') -Environment $runEnvironment -TimeoutSeconds $DurationSeconds
                    $runs.Add([ordered]@{ name = $name; console = $mode; loop_limit = $limit; skipped_pipelines = @($variant.skip); process = $result })
                    # The operator records what actually appeared. Never infer
                    # a playable scene from an exit code or a compiler result.
                    Write-VictoryJson (Join-Path $runDir 'observations.json') ([ordered]@{
                        stage_reached = 'not-recorded'
                        recognizable_scene = $null
                        character_control = $null
                        driver_reset_observed = $null
                        notes = ''
                    })
                    Write-VictoryJson (Join-Path $report 'summary.json') $summary
                    if ($result.error -or (-not $result.timed_out -and $result.exit_code -ne 0)) {
                        throw "Stopped matrix after a failed run: $name. Inspect its process report and logs before rerunning."
                    }
                }
            }
        }
        $summary.status = 'collected-unverified'
    }
} catch {
    $summary.status = 'failed'
    $summary['error'] = $_.Exception.Message
    throw
} finally {
    $summary['finished_at_utc'] = [DateTime]::UtcNow.ToString('o')
    Write-VictoryJson (Join-Path $report 'summary.json') $summary
    Write-Host "Diagnostics report: $report"
}
