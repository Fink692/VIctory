#Requires -Version 7.2
# Shared process and path handling for the Windows validation tools.
Set-StrictMode -Version Latest

$script:VictoryRepoRoot = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))

function Assert-VictoryExternalPath {
    param([Parameter(Mandatory)][string]$Path)
    $full = [IO.Path]::GetFullPath($Path, $PWD.ProviderPath)
    $comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    $root = $script:VictoryRepoRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    if ($full.Equals($root, $comparison) -or $full.StartsWith($root + [IO.Path]::DirectorySeparatorChar, $comparison)) {
        throw "Diagnostics output and game data must be outside the repository: $full"
    }
    # Reject junctions/symlinks instead of trusting a lexical path comparison.
    # Also catches another checkout or an archive copy carrying the marker.
    for ($at = $full; $at; $at = [IO.Path]::GetDirectoryName($at)) {
        if (Test-Path -LiteralPath $at) {
            $item = Get-Item -LiteralPath $at -Force -ErrorAction Stop
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Use a direct path, without junctions or symbolic links: $at"
            }
            if ($item.PSIsContainer -and (Test-Path -LiteralPath (Join-Path $at '.victory-root'))) {
                throw "Path is inside a VIctory source tree: $full"
            }
        }
    }
    return $full
}

function New-VictoryReportDirectory {
    param([Parameter(Mandatory)][string]$OutputRoot, [string]$Prefix = 'run')
    $root = Assert-VictoryExternalPath $OutputRoot
    $name = '{0}-{1}-{2}' -f $Prefix, ([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')), ([Guid]::NewGuid().ToString('N').Substring(0, 8))
    $path = Assert-VictoryExternalPath (Join-Path $root $name)
    [void][IO.Directory]::CreateDirectory($path)
    return $path
}

function Write-VictoryJson {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    $full = Assert-VictoryExternalPath $Path
    $Value | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $full -Encoding utf8
}

function Invoke-VictoryProcess {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Executable,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory)][string]$WorkingDirectory,
        [Parameter(Mandatory)][string]$LogPrefix,
        [hashtable]$Environment = @{},
        [ValidateRange(1, 7200)][int]$TimeoutSeconds = 600
    )
    $prefix = Assert-VictoryExternalPath $LogPrefix
    [void][IO.Directory]::CreateDirectory((Split-Path $prefix -Parent))
    foreach ($suffix in @('.stdout.log', '.stderr.log', '.process.json')) {
        if (Test-Path -LiteralPath ($prefix + $suffix)) { throw "Log prefix already exists: $prefix" }
    }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.WorkingDirectory = $WorkingDirectory
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    # Never accidentally run with inherited title paths, dump switches, SDK
    # overrides or a different adapter. Only the caller's explicit set survives.
    foreach ($key in @($info.Environment.Keys)) {
        if ($key.StartsWith('VICTORY_', [StringComparison]::OrdinalIgnoreCase)) {
            [void]$info.Environment.Remove($key)
        }
    }
    foreach ($key in $Environment.Keys) { $info.Environment[$key] = [string]$Environment[$key] }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $outFile = $null
    $errFile = $null
    $started = $false
    $outcome = [ordered]@{
        schema_version = 1
        executable = $Executable
        arguments = @($Arguments)
        started_at_utc = [DateTime]::UtcNow.ToString('o')
        timeout_seconds = $TimeoutSeconds
        duration_ms = 0
        exit_code = $null
        timed_out = $false
        output_complete = $false
        error = $null
        stdout_log = "$prefix.stdout.log"
        stderr_log = "$prefix.stderr.log"
    }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        # CreateNew prevents accidental replacement of a previous measurement.
        $outFile = [IO.File]::Open($outcome.stdout_log, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $errFile = [IO.File]::Open($outcome.stderr_log, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $started = $process.Start()
        if (-not $started) { throw "Could not start $Executable" }
        # Drain both pipes concurrently, directly to disk, avoiding deadlocks
        # and unbounded in-memory buffering on verbose diagnostics runs.
        $stdout = $process.StandardOutput.BaseStream.CopyToAsync($outFile)
        $stderr = $process.StandardError.BaseStream.CopyToAsync($errFile)
        while (-not $process.WaitForExit(250)) {
            if ($clock.Elapsed.TotalSeconds -ge $TimeoutSeconds) {
                $outcome.timed_out = $true
                $process.Kill($true)
                if (-not $process.WaitForExit(10000)) { throw 'Process did not exit after termination.' }
                break
            }
        }
        $outcome.exit_code = $process.ExitCode
        $outcome.output_complete = [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($stdout, $stderr), 5000)
        if (-not $outcome.output_complete) {
            $outcome.error = 'Output pipes remained open after exit (possibly a relaunched child); log capture is incomplete.'
        }
    } catch {
        $outcome.error = $_.Exception.Message
    } finally {
        if ($started) {
            if (-not $process.HasExited) {
                try { $process.Kill($true); [void]$process.WaitForExit(10000) } catch { }
            }
            $process.StandardOutput.Dispose()
            $process.StandardError.Dispose()
        }
        if ($outFile) { $outFile.Dispose() }
        if ($errFile) { $errFile.Dispose() }
        $process.Dispose()
        $clock.Stop()
        $outcome.duration_ms = $clock.ElapsedMilliseconds
        Write-VictoryJson "$prefix.process.json" $outcome
    }
    return [pscustomobject]$outcome
}

function Assert-VictoryProcessSucceeded {
    param([Parameter(Mandatory)]$Result, [Parameter(Mandatory)][string]$Label)
    if ($Result.error -or $Result.timed_out -or $Result.exit_code -ne 0) {
        throw "$Label failed (exit=$($Result.exit_code), timeout=$($Result.timed_out), error=$($Result.error)). See $($Result.stderr_log) and $($Result.stdout_log)."
    }
}

function Get-VictoryWarpModuleEvidence {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$Output,
        [string]$ExpectedPath,
        [string]$ExpectedVersion
    )
    if ([bool]$ExpectedPath -ne [bool]$ExpectedVersion) {
        throw 'Provide both the expected WARP DLL path and file version.'
    }
    $blocks = [regex]::Matches($Output, '(?ms)^VICTORY_WARP_MODULE_BEGIN\r?\n(.*?)^VICTORY_WARP_MODULE_END\r?$')
    if ($blocks.Count -eq 0) { throw 'The debugger did not capture the loaded WARP module.' }
    $observed = [Collections.Generic.List[object]]::new()
    foreach ($block in $blocks) {
        $pathMatch = [regex]::Match($block.Groups[1].Value, '(?im)^\s*Image path:\s*(.+?d3d10warp\.dll)\s*$')
        $versionMatch = [regex]::Match($block.Groups[1].Value, '(?im)^\s*File version:\s*([\d.]+)\s*$')
        if (-not $pathMatch.Success -or -not $versionMatch.Success) { throw 'WARP module path/version evidence is incomplete.' }
        $path = $pathMatch.Groups[1].Value.Trim()
        $version = $versionMatch.Groups[1].Value
        # Without an expected runtime, still require every captured load to
        # agree. CDB may repeat the same module block in stdout and its log.
        if (-not $ExpectedPath) {
            $ExpectedPath = $path
            $ExpectedVersion = $version
        }
        if (-not [string]::Equals($path, $ExpectedPath, [StringComparison]::OrdinalIgnoreCase) -or $version -ne $ExpectedVersion) {
            throw "Unexpected loaded WARP: $path version $version; expected $ExpectedPath version $ExpectedVersion"
        }
        $observed.Add([ordered]@{ path = $path; file_version = $version })
    }
    return $observed[0]
}

function Get-VictoryAtomic64Coverage {
    param([Parameter(Mandatory)][string]$Output)
    # The semantic test emits this only after all supported cases complete,
    # or after an explicit capability check finds no supported adapter.
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_ATOMIC64_RESULT=(executed|unsupported) adapters=(\d+) cases=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1) { throw 'The atomic64 test must report exactly one execution/capability result.' }
    $status = $coverageMatches[0].Groups[1].Value
    $adapters = [uint64]$coverageMatches[0].Groups[2].Value
    $cases = [uint64]$coverageMatches[0].Groups[3].Value
    if (($status -eq 'executed' -and ($adapters -eq 0 -or $cases -eq 0)) -or
        ($status -eq 'unsupported' -and ($adapters -ne 0 -or $cases -ne 0))) {
        throw 'The atomic64 test reported contradictory execution/capability counts.'
    }
    return [ordered]@{ status = $status; adapters = $adapters; cases = $cases }
}

function Get-VictoryAtomicValidationCoverage {
    param([Parameter(Mandatory)][string]$Output)
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_ATOMIC_VALIDATION_RESULT=validated containers=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1 -or [uint64]$coverageMatches[0].Groups[1].Value -ne 88) {
        throw 'The atomic validation test must confirm independent validation of all 88 synthetic containers.'
    }
    return [ordered]@{ status = 'validated'; containers = 88 }
}

function Get-VictoryLdsValidationCoverage {
    param([Parameter(Mandatory)][string]$Output)
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_LDS_VALIDATION_RESULT=validated containers=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1 -or [uint64]$coverageMatches[0].Groups[1].Value -ne 168) {
        throw 'The LDS validation test must confirm independent validation of all 168 synthetic containers.'
    }
    return [ordered]@{ status = 'validated'; containers = 168 }
}

function Get-VictoryShaderHashCoverage {
    param([Parameter(Mandatory)][string]$Output)
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_SHADER_HASH_RESULT=matched programs=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1 -or [uint64]$coverageMatches[0].Groups[1].Value -ne 2) {
        throw 'Shader hashes must match the official compiler for both authored reference programs.'
    }
    return [ordered]@{ status = 'matched'; programs = 2 }
}

function Get-VictoryLdsCoverage {
    param([Parameter(Mandatory)][string]$Output)
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_LDS_RESULT=executed adapters=(\d+) cases=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1) { throw 'The LDS test must report exactly one execution result.' }
    $adapters = [uint64]$coverageMatches[0].Groups[1].Value
    $cases = [uint64]$coverageMatches[0].Groups[2].Value
    if ($adapters -eq 0 -or $cases -ne ($adapters * 496)) {
        throw 'The LDS test must execute all 496 cases on each reported adapter.'
    }
    return [ordered]@{ status = 'executed'; adapters = $adapters; cases = $cases }
}

function Get-VictoryDepthBoundsCoverage {
    param([Parameter(Mandatory)][string]$Output)
    $coverageMatches = [regex]::Matches($Output, '(?m)^VICTORY_DEPTH_BOUNDS_RESULT=(executed|unsupported) cases=(\d+)\r?$')
    if ($coverageMatches.Count -ne 1) { throw 'The depth-bounds test must report exactly one execution/capability result.' }
    $status = $coverageMatches[0].Groups[1].Value
    $cases = [uint64]$coverageMatches[0].Groups[2].Value
    if (($status -eq 'executed' -and $cases -ne 14) -or ($status -eq 'unsupported' -and $cases -ne 0)) {
        throw 'The depth-bounds test must execute all 14 cases or report unsupported with zero executed cases.'
    }
    return [ordered]@{ status = $status; cases = $cases }
}

function Get-VictoryNativeShaderCoverage {
    param([Parameter(Mandatory)][string]$Output)
    if ($Output -notmatch '(?m)^VICTORY_NATIVE_DEBUGGER_READY\r?$') {
        throw 'The native debugger did not confirm its exception configuration.'
    }
    if ($Output -match '(?m)^VICTORY_NATIVE_UNHANDLED\r?$' -or $Output -match 'test result: FAILED\.') {
        throw 'The native shader run reported an unhandled exception or failed Rust tests.'
    }
    # CDB can echo the same terminal result into both its own log and stdout.
    # Deduplicate those copies, but reject different or missing terminal results.
    $results = @([regex]::Matches($Output, '(?m)^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;[^\r\n]*') | ForEach-Object { $_.Value } | Select-Object -Unique)
    if ($results.Count -ne 1 -or $results[0] -notmatch '^test result: ok\. ([1-9]\d*) passed; 0 failed; (\d+) ignored;') {
        throw 'The native shader run did not complete exactly one successful Rust test suite.'
    }
    return [ordered]@{ status = 'completed'; passed = [uint32]$Matches[1]; ignored = [uint32]$Matches[2] }
}
