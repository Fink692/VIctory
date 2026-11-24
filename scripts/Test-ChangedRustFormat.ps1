#Requires -Version 7.2
<#
.SYNOPSIS
Require complete changed Rust files to match rustfmt without reformatting
unrelated legacy sources. A rustfmt configuration change requires the full gate.
#>
[CmdletBinding()]
param(
    [string]$BaseRef = 'origin/main',
    [string]$Repository = (Split-Path $PSScriptRoot -Parent)
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = [IO.Path]::GetFullPath($Repository, $PWD.ProviderPath)

function Read-Git {
    param([string[]]$Arguments)
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = 'git'
    $info.WorkingDirectory = $root
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $p = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $p.StandardOutput.ReadToEndAsync()
        $stderr = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) { $p.Kill($true); throw 'Timed out reading local git metadata.' }
        $text = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        if ($p.ExitCode -ne 0) { throw "git $($Arguments -join ' ') failed: $errorText" }
        return $text
    } finally { $p.Dispose() }
}

# Fail for a missing/shallow base instead of silently checking an empty list.
$base = (Read-Git @('rev-parse', '--verify', '--end-of-options', "$BaseRef^{commit}")).Trim()
Write-Host "Formatting base: $BaseRef ($base)"
# Comparing the base to the working tree includes committed, staged and
# unstaged edits. --no-renames makes a renamed destination a normal changed file.
$changed = @((Read-Git @('diff', '--name-only', '--no-renames', '-z', $base, '--')) -split "`0" | Where-Object { $_ })
$untracked = @((Read-Git @('ls-files', '--others', '--exclude-standard', '-z')) -split "`0" | Where-Object { $_ })
$paths = @(@($changed) + @($untracked) | Sort-Object -Unique)
Push-Location -LiteralPath $root
try {
    if (@($paths | Where-Object { $_ -match '(^|/)\.?rustfmt\.toml$' }).Count -gt 0) {
        Write-Host 'Formatter configuration changed: checking the complete workspace.'
        & cargo fmt --all --check
        if ($LASTEXITCODE -ne 0) { throw 'Workspace formatting check failed.' }
        return
    }
    $rust = @($paths | Where-Object { $_.EndsWith('.rs', [StringComparison]::Ordinal) -and (Test-Path -LiteralPath (Join-Path $root $_) -PathType Leaf) })
    if ($rust.Count -eq 0) {
        Write-Host 'No Rust files differ from this base. No Rust formatting was exercised; this is not a full-workspace formatting pass.'
        return
    }
    $manifest = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
    $package = [regex]::Match($manifest, '(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\z)').Groups[1].Value
    $edition = [regex]::Match($package, '(?m)^edition\s*=\s*"(\d+)"').Groups[1].Value
    if (-not $edition) { throw 'Cannot determine the workspace Rust edition; refusing to guess.' }
    $failed = [Collections.Generic.List[string]]::new()
    foreach ($file in $rust) {
        Write-Host "Checking full file: $file"
        # skip_children prevents mod declarations from pulling unchanged
        # modules into this gate. Each changed module is checked in its own turn.
        & rustfmt --check --edition $edition --config-path (Join-Path $root 'rustfmt.toml') --config skip_children=true (Join-Path $root $file)
        if ($LASTEXITCODE -ne 0) { $failed.Add($file) }
    }
    if ($failed.Count -gt 0) { throw "Formatting failed for $($failed.Count) changed Rust file(s): $($failed -join ', ')" }
    Write-Host "Formatting passed for $($rust.Count) complete changed Rust files. Untouched baseline formatting debt remains outside this gate."
} finally { Pop-Location }
