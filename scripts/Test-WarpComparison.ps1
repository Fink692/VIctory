#Requires -Version 7.2
<#
.SYNOPSIS
Replay native diagnostic controls against inbox and pinned official WARP.
.DESCRIPTION
Controlled diagnostic experiment. Takes the existing native-shader/native-gpu reports
from this checkout and reuses their exact test executables and test arguments.
The repository and its target directory are not modified. Separate copies of
the same executable bytes run from isolated inbox/pinned application folders.
All failures remain failures, and only text diagnostics are collected.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$ShaderReportPath,
    [Parameter(Mandatory)][string]$GpuReportPath,
    [string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) 'victory-warp-comparison'),
    [string]$PackagePath,
    [string]$CdbPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or -not [Environment]::Is64BitProcess) { throw 'WARP comparison requires Windows x64.' }
$RepositoryRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
. (Join-Path $RepositoryRoot 'scripts/Diagnostics.Common.ps1')
$ReportPaths = @($ShaderReportPath, $GpuReportPath)
$report = New-VictoryReportDirectory $OutputRoot 'warp-comparison'
$packageUrl = 'https://api.nuget.org/v3-flatcontainer/microsoft.direct3d.warp/1.0.22/microsoft.direct3d.warp.1.0.22.nupkg'
$packageSha256 = 'afc058f4d1066edc4231a38f3ab3592b1930f7287d4ac700e6a056e0874bb0be'
$dllSha256 = 'bb8ee56fd0f7ee665abb61178301e612181e5e2f57af0e5ab64e456b8f1a8c4f'
$runs = [Collections.Generic.List[object]]::new()
$failures = [Collections.Generic.List[string]]::new()
$summary = [ordered]@{
    schema_version = 1
    started_at_utc = [DateTime]::UtcNow.ToString('o')
    status = 'running'
    package_url = $packageUrl
    package_sha256 = $packageSha256
    pinned_dll_sha256 = $dllSha256
    reports = @($ReportPaths)
    runs = $runs
    limitations = @('Controlled replay of synthetic tests; no process dumps.', 'A passing pinned-runtime control does not erase a failed inbox-runtime control.')
}

function Assert-FileHash {
    param([string]$Path, [string]$Expected)
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Expected.ToLowerInvariant()) { throw "Unexpected SHA256 for $Path" }
}

function Get-WarpModuleEvidence {
    param([string]$Output, [string]$ExpectedPath, [string]$ExpectedVersion)
    $blocks = [regex]::Matches($Output, '(?ms)^VICTORY_WARP_MODULE_BEGIN\r?\n(.*?)^VICTORY_WARP_MODULE_END\r?$')
    if ($blocks.Count -eq 0) { throw 'The debugger did not capture the loaded WARP module.' }
    $observed = [Collections.Generic.List[object]]::new()
    foreach ($block in $blocks) {
        $pathMatch = [regex]::Match($block.Groups[1].Value, '(?im)^\s*Image path:\s*(.+?d3d10warp\.dll)\s*$')
        $versionMatch = [regex]::Match($block.Groups[1].Value, '(?im)^\s*File version:\s*([\d.]+)\s*$')
        if (-not $pathMatch.Success -or -not $versionMatch.Success) { throw 'WARP module path/version evidence is incomplete.' }
        $path = $pathMatch.Groups[1].Value.Trim()
        $version = $versionMatch.Groups[1].Value
        if (-not [string]::Equals($path, $ExpectedPath, [StringComparison]::OrdinalIgnoreCase) -or $version -ne $ExpectedVersion) {
            throw "Unexpected loaded WARP: $path version $version; expected $ExpectedPath version $ExpectedVersion"
        }
        $observed.Add([ordered]@{ path = $path; file_version = $version })
    }
    return $observed[0]
}

try {
    if (-not $CdbPath) {
        $command = Get-Command cdb.exe -ErrorAction SilentlyContinue
        if ($command) { $CdbPath = $command.Source }
        else { $CdbPath = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/Debuggers/x64/cdb.exe' }
    }
    if (-not (Test-Path -LiteralPath $CdbPath -PathType Leaf)) { throw 'The Windows SDK x64 debugger is required.' }
    $CdbPath = [IO.Path]::GetFullPath($CdbPath)
    if (-not $PackagePath) {
        $PackagePath = Join-Path $report 'Microsoft.Direct3D.WARP.1.0.22.nupkg'
        Invoke-WebRequest -Uri $packageUrl -OutFile $PackagePath -TimeoutSec 60
    }
    Assert-FileHash $PackagePath $packageSha256
    $applicationDirectories = @{}
    foreach ($runtime in @('inbox', 'pinned')) {
        $directory = Join-Path $report "$runtime-application"
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        $applicationDirectories[$runtime] = $directory
    }
    $archive = [IO.Compression.ZipFile]::OpenRead($PackagePath)
    try {
        foreach ($file in @('d3d10warp.dll', 'd3d10warp.pdb')) {
            $entry = $archive.GetEntry("build/native/bin/x64/$file")
            if (-not $entry) { throw "The pinned package lacks x64/$file." }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $applicationDirectories.pinned $file), $false)
        }
    } finally { $archive.Dispose() }
    $pinnedDll = Join-Path $applicationDirectories.pinned 'd3d10warp.dll'
    Assert-FileHash $pinnedDll $dllSha256
    $pinnedVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($pinnedDll).FileVersionRaw.ToString()
    if ($pinnedVersion -ne '1.0.22.0') { throw "Unexpected pinned WARP version $pinnedVersion" }
    $signature = Get-AuthenticodeSignature -LiteralPath $pinnedDll
    $summary['pinned_authenticode'] = [ordered]@{
        status = $signature.Status.ToString()
        signer_subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
    }
    $inboxDll = Join-Path ([Environment]::SystemDirectory) 'd3d10warp.dll'
    $inboxVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($inboxDll).FileVersionRaw.ToString()
    $summary['inbox_dll'] = [ordered]@{ path = $inboxDll; file_version = $inboxVersion; sha256 = (Get-FileHash -LiteralPath $inboxDll -Algorithm SHA256).Hash.ToLowerInvariant() }
    $summary['pinned_dll'] = [ordered]@{ path = $pinnedDll; file_version = $pinnedVersion; sha256 = $dllSha256 }
    $commands = Join-Path $report 'debugger-commands.txt'
    $exceptionCommands = '.echo VICTORY_NATIVE_UNHANDLED; .lastevent; q'
    @"
sxd -c2 "$exceptionCommands" av
sxd -c2 "$exceptionCommands" 0xc0000409
sxn out
sxe -c ".echo VICTORY_WARP_MODULE_BEGIN; lmvm d3d10warp; .echo VICTORY_WARP_MODULE_END; g" ld:*d3d10warp*
.echo VICTORY_NATIVE_DEBUGGER_READY
g
"@ | Set-Content -LiteralPath $commands -Encoding ascii
    $config = Join-Path $report 'victory.toml'
    '[paths]' | Set-Content -LiteralPath $config -Encoding utf8
    foreach ($reportPath in $ReportPaths) {
        $source = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
        $kind = switch ([IO.Path]::GetFileName($reportPath)) {
            'native-shader.json' { 'shader' }
            'native-gpu.json' { 'gpu' }
            default { throw 'Provide only native-shader.json and native-gpu.json reports from this checkout.' }
        }
        $originalExecutable = $source.test_executable
        if (-not (Test-Path -LiteralPath $originalExecutable -PathType Leaf)) { throw "Missing original test executable $originalExecutable" }
        $originalHash = (Get-FileHash -LiteralPath $originalExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
        foreach ($runtime in @('inbox', 'pinned')) {
            $applicationDirectory = $applicationDirectories[$runtime]
            $executable = Join-Path $applicationDirectory ([IO.Path]::GetFileName($originalExecutable))
            Copy-Item -LiteralPath $originalExecutable -Destination $executable
            Assert-FileHash $executable $originalHash
            $pdb = [IO.Path]::ChangeExtension($originalExecutable, '.pdb')
            if (Test-Path -LiteralPath $pdb) { Copy-Item -LiteralPath $pdb -Destination $applicationDirectory }
            $symbols = $applicationDirectory + ';' + (Split-Path $originalExecutable -Parent) + ';srv*' + (Join-Path $report 'symbols') + '*https://msdl.microsoft.com/download/symbols'
            foreach ($sourceRun in $source.runs) {
                $label = "$runtime-$kind-$($sourceRun.label)"
                $run = [ordered]@{ label = $label; runtime = $runtime; kind = $kind; status = 'running'; executable_sha256 = $originalHash }
                $runs.Add($run)
                try {
                    $originalArguments = @($sourceRun.process.arguments)
                    $index = [Array]::IndexOf($originalArguments, $originalExecutable)
                    if ($index -lt 0 -or $index -eq $originalArguments.Count - 1) { throw 'Original CDB arguments do not identify executable and tests.' }
                    $testArguments = @($originalArguments[($index + 1)..($originalArguments.Count - 1)])
                    $prefix = Join-Path $report $label
                    $nativeLog = "$prefix.native.log"
                    $arguments = @('-G', '-lines', '-y', $symbols, '-logo', $nativeLog, '-cf', $commands, $executable) + $testArguments
                    $environment = @{
                        VICTORY_CONFIG = $config
                        VICTORY_CACHE_DIR = (Join-Path $report "$label-cache")
                        VICTORY_DATA_DIR = (Join-Path $report "$label-data")
                        VICTORY_WARP = '1'
                        VICTORY_D3D12_WARP = '1'
                        VICTORY_ATOMIC_TRACE = '1'
                        VICTORY_LDS_TRACE = '1'
                        VICTORY_COPY_TRACE = '1'
                        VICTORY_DEPTH_BOUNDS_TRACE = '1'
                        CARGO_TERM_COLOR = 'never'
                    }
                    $environment.VICTORY_D3D12_DEBUG = if ($kind -eq 'gpu') { '1' } else { '0' }
                    if ($env:VICTORY_DXIL) { $environment.VICTORY_DXIL = $env:VICTORY_DXIL }
                    $result = Invoke-VictoryProcess -Executable $CdbPath -Arguments $arguments -WorkingDirectory $RepositoryRoot -LogPrefix $prefix -Environment $environment -TimeoutSeconds 300
                    $run['process'] = $result
                    $output = (Get-Content -LiteralPath $result.stdout_log -Raw) + (Get-Content -LiteralPath $result.stderr_log -Raw)
                    if (Test-Path -LiteralPath $nativeLog) { $output += Get-Content -LiteralPath $nativeLog -Raw }
                    $expectedPath = if ($runtime -eq 'pinned') { $pinnedDll } else { $inboxDll }
                    $expectedVersion = if ($runtime -eq 'pinned') { $pinnedVersion } else { $inboxVersion }
                    $run['loaded_warp'] = Get-WarpModuleEvidence $output $expectedPath $expectedVersion
                    Assert-VictoryProcessSucceeded $result $label
                    $run['coverage'] = Get-VictoryNativeShaderCoverage $output
                    $expectedTests = if ($kind -eq 'gpu' -or $sourceRun.label -eq 'isolated-lds') { 1 } elseif ($sourceRun.label -eq 'isolated-valu') { 7 } else { 0 }
                    if ($expectedTests -and ($run.coverage.passed -ne $expectedTests -or $run.coverage.ignored -ne 0)) { throw "Unexpected test completion counts for $label" }
                    if ($sourceRun.label -eq 'isolated-lds') {
                        $markers = @([regex]::Matches($output, '(?m)^VICTORY_LDS_RESULT=[^\r\n]*') | ForEach-Object { $_.Value } | Select-Object -Unique)
                        $run['semantic_coverage'] = Get-VictoryLdsCoverage ($markers -join "`n")
                    } elseif ($sourceRun.label -eq 'depth-bounds') {
                        $markers = @([regex]::Matches($output, '(?m)^VICTORY_DEPTH_BOUNDS_RESULT=[^\r\n]*') | ForEach-Object { $_.Value } | Select-Object -Unique)
                        $run['semantic_coverage'] = Get-VictoryDepthBoundsCoverage ($markers -join "`n")
                    }
                    $run.status = 'passed'
                    Write-Host "$label completed using $($run.loaded_warp.path) version $($run.loaded_warp.file_version)"
                } catch {
                    $run.status = 'failed'
                    $run['error'] = $_.Exception.Message
                    $failures.Add($label)
                    Write-Host "$label failed: $($run.error)"
                }
            }
        }
    }
    if ($runs.Count -eq 0) { throw 'No native controls were replayed.' }
    if ($failures.Count) { throw "WARP comparison controls failed: $($failures -join ', ')" }
    $summary.status = 'passed'
} catch {
    $summary.status = 'failed'
    $summary['error'] = $_.Exception.Message
    throw
} finally {
    $summary['finished_at_utc'] = [DateTime]::UtcNow.ToString('o')
    Write-VictoryJson (Join-Path $report 'warp-comparison.json') $summary
    Write-Host "WARP comparison report: $report"
}
