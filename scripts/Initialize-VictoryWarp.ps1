#Requires -Version 7.2
<#
.SYNOPSIS
Prepare the pinned Windows x64 WARP development runtime beside test executables.
.DESCRIPTION
Dot-source this file to call Get-VictoryWarpPackage (prepare only) or
Initialize-VictoryWarp (prepare and install). Direct invocation calls the latter.
Package/cache and optional JSON report paths must be outside source trees.
ApplicationDirectory must name the actual executable directory; Cargo test
executables normally live under target/debug/deps, not target/debug.

Only d3d10warp.dll, its matching PDB, and the package license are copied.
Matching files are reused. Different existing files are rejected unless the
caller explicitly selects ExistingDllPolicy ReplaceDifferent. No system DLLs
are changed. This package is for internal Windows development/testing under
its accompanying license; do not redistribute it with VIctory or diagnostics.

Returned metadata proves the bytes installed, not which module a process loads.
Keep loaded-module evidence in native diagnostics when that distinction matters.
#>
[CmdletBinding()]
param(
    [string[]]$ApplicationDirectory,
    [string]$CacheDirectory,
    [string]$PackagePath,
    [string]$ReportPath,
    [ValidateSet('RefuseDifferent', 'ReplaceDifferent')]
    [string]$ExistingDllPolicy = 'RefuseDifferent'
)

function Get-VictoryWarpIdentity {
    # Official package and catalog, also used by Test-WarpComparison.ps1:
    # https://www.nuget.org/packages/Microsoft.Direct3D.WARP/1.0.22
    # https://api.nuget.org/v3/catalog0/data/2026.10.28.17.01.53/microsoft.direct3d.warp.1.0.22.json
    [pscustomobject]@{
        package_name = 'Microsoft.Direct3D.WARP'
        package_version = '1.0.22'
        package_url = 'https://api.nuget.org/v3-flatcontainer/microsoft.direct3d.warp/1.0.22/microsoft.direct3d.warp.1.0.22.nupkg'
        package_sha256 = 'afc058f4d1066edc4231a38f3ab3592b1930f7287d4ac700e6a056e0874bb0be'
        package_sha512_base64 = 'mxnDzBCKPqzwYwxlvWHyH+qjSBSPCDecHVXI0gXsfqEjIYH+yX9oYX6VQCVqjEpQUVtwrQGCBr3hVrQXDUfQ1w=='
        dll_version = '1.0.22.0'
        files = @(
            [pscustomobject]@{ kind = 'dll'; name = 'd3d10warp.dll'; entry = 'build/native/bin/x64/d3d10warp.dll'; sha256 = 'bb8ee56fd0f7ee665abb61178301e612181e5e2f57af0e5ab64e456b8f1a8c4f' }
            [pscustomobject]@{ kind = 'pdb'; name = 'd3d10warp.pdb'; entry = 'build/native/bin/x64/d3d10warp.pdb'; sha256 = '69a0dabe725b433c6f3f3b62c420e3ec08e7ab7b1219a419ff34040470f0dcc9' }
            [pscustomobject]@{ kind = 'license'; name = 'Microsoft.Direct3D.WARP.LICENSE.txt'; entry = 'LICENSE.TXT'; sha256 = '5435f10305a92870b45735dfc169c5d6010617f6556e0859689a058d2c6b59c4' }
        )
    }
}

function Assert-VictoryWarpPlatform {
    if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
        throw 'The pinned WARP development runtime requires a Windows x64 process.'
    }
}

function Resolve-VictoryWarpWritePath {
    param([Parameter(Mandatory)][string]$Path, [switch]$External)
    $full = [IO.Path]::GetFullPath($Path, $PWD.ProviderPath)
    $comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    $separator = [IO.Path]::DirectorySeparatorChar
    $repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..')).TrimEnd($separator, [IO.Path]::AltDirectorySeparatorChar)
    $windowsDirectories = @($env:SystemRoot, $env:WINDIR, [Environment]::GetFolderPath([Environment+SpecialFolder]::Windows))
    foreach ($directory in $windowsDirectories) {
        if (-not $directory) { continue }
        $directory = [IO.Path]::GetFullPath($directory).TrimEnd($separator, [IO.Path]::AltDirectorySeparatorChar)
        if ($full.Equals($directory, $comparison) -or $full.StartsWith($directory + $separator, $comparison)) {
            throw "WARP preparation must not write inside the Windows installation: $full"
        }
    }
    # Check ancestors as well as the final component: junctions must not turn a
    # build/cache destination into a write into source files or the system tree.
    for ($at = $full; $at; $at = [IO.Path]::GetDirectoryName($at)) {
        if (Test-Path -LiteralPath $at) {
            $item = Get-Item -LiteralPath $at -Force -ErrorAction Stop
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Use a direct WARP path without junctions or symbolic links: $at"
            }
        }
        $isSourceTree = $at.Equals($repository, $comparison) -or (Test-Path -LiteralPath (Join-Path $at '.victory-root'))
        if ($isSourceTree) {
            $target = Join-Path $at 'target'
            $isBuildOutput = $full.Equals($target, $comparison) -or $full.StartsWith($target + $separator, $comparison)
            if ($External -or -not $isBuildOutput) {
                throw "WARP cache/reports must be external; source-tree installs are allowed only under target: $full"
            }
        }
    }
    return $full
}

function Assert-VictoryWarpHash {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Expected)
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
    if ($actual -ne $Expected) { throw "WARP SHA256 mismatch for $Path (expected $Expected; found $actual)." }
}

function Assert-VictoryWarpPackageHash {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Identity)
    Assert-VictoryWarpHash $Path $Identity.package_sha256
    $sha512 = (Get-FileHash -LiteralPath $Path -Algorithm SHA512 -ErrorAction Stop).Hash
    $base64 = [Convert]::ToBase64String([Convert]::FromHexString($sha512))
    if ($base64 -cne $Identity.package_sha512_base64) { throw "WARP package SHA512 does not match the official catalog: $Path" }
}

function Get-VictoryWarpDllVersion {
    param([Parameter(Mandatory)][string]$Path)
    $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($Path)
    $expected = (Get-VictoryWarpIdentity).dll_version
    if ($version.FileVersionRaw.ToString() -ne $expected -or $version.ProductVersionRaw.ToString() -ne $expected) {
        throw "Unexpected WARP DLL version at $Path (file $($version.FileVersionRaw); product $($version.ProductVersionRaw))."
    }
    return $version.FileVersionRaw.ToString()
}

function Get-VictoryWarpPackage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$CacheDirectory,
        [string]$PackagePath
    )
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    Assert-VictoryWarpPlatform
    $identity = Get-VictoryWarpIdentity
    $cache = Resolve-VictoryWarpWritePath $CacheDirectory -External
    $directory = Resolve-VictoryWarpWritePath (Join-Path $cache 'Microsoft.Direct3D.WARP/1.0.22') -External
    $cachedPackage = Resolve-VictoryWarpWritePath (Join-Path $directory 'Microsoft.Direct3D.WARP.1.0.22.nupkg') -External
    if ($PackagePath) {
        $PackagePath = (Resolve-Path -LiteralPath $PackagePath -ErrorAction Stop).ProviderPath
        Assert-VictoryWarpPackageHash $PackagePath $identity
    }
    [void][IO.Directory]::CreateDirectory($directory)
    if (Test-Path -LiteralPath $cachedPackage) {
        Assert-VictoryWarpPackageHash $cachedPackage $identity
    } else {
        $temporary = Join-Path $directory ([Guid]::NewGuid().ToString('N') + '.nupkg.partial')
        try {
            if ($PackagePath) { [IO.File]::Copy($PackagePath, $temporary, $false) }
            else { Invoke-WebRequest -Uri $identity.package_url -OutFile $temporary -TimeoutSec 60 }
            Assert-VictoryWarpPackageHash $temporary $identity
            [IO.File]::Move($temporary, $cachedPackage, $false)
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        }
    }
    $payload = Resolve-VictoryWarpWritePath (Join-Path $directory 'x64') -External
    [void][IO.Directory]::CreateDirectory($payload)
    $files = [Collections.Generic.List[object]]::new()
    $archive = [IO.Compression.ZipFile]::OpenRead($cachedPackage)
    try {
        foreach ($file in $identity.files) {
            $path = Resolve-VictoryWarpWritePath (Join-Path $payload $file.name) -External
            if (Test-Path -LiteralPath $path) {
                Assert-VictoryWarpHash $path $file.sha256
            } else {
                $entry = $archive.GetEntry($file.entry)
                if (-not $entry) { throw "The verified WARP package lacks $($file.entry)." }
                $temporary = Join-Path $payload ([Guid]::NewGuid().ToString('N') + '.partial')
                try {
                    [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $temporary, $false)
                    Assert-VictoryWarpHash $temporary $file.sha256
                    [IO.File]::Move($temporary, $path, $false)
                } finally {
                    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
                }
            }
            $files.Add([pscustomobject]@{ kind = $file.kind; name = $file.name; path = $path; sha256 = $file.sha256 })
        }
    } finally { $archive.Dispose() }
    $dll = @($files | Where-Object kind -EQ 'dll')[0]
    $version = Get-VictoryWarpDllVersion $dll.path
    [pscustomobject]@{
        package_name = $identity.package_name
        package_version = $identity.package_version
        package_url = $identity.package_url
        package_path = $cachedPackage
        package_sha256 = $identity.package_sha256
        package_sha512_base64 = $identity.package_sha512_base64
        architecture = 'x64'
        dll_version = $version
        files = $files.ToArray()
    }
}

function Initialize-VictoryWarp {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string[]]$ApplicationDirectory,
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$CacheDirectory,
        [string]$PackagePath,
        [string]$ReportPath,
        [ValidateSet('RefuseDifferent', 'ReplaceDifferent')]
        [string]$ExistingDllPolicy = 'RefuseDifferent'
    )
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    Assert-VictoryWarpPlatform
    $directories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($directory in $ApplicationDirectory) {
        [void]$directories.Add((Resolve-VictoryWarpWritePath $directory))
    }
    if ($ReportPath) {
        $ReportPath = Resolve-VictoryWarpWritePath $ReportPath -External
        if ([IO.Path]::GetExtension($ReportPath) -ine '.json') { throw 'The WARP report path must end in .json.' }
    }
    $prepared = Get-VictoryWarpPackage -CacheDirectory $CacheDirectory -PackagePath $PackagePath
    $pending = [Collections.Generic.List[object]]::new()
    # Preflight every destination before creating or replacing any application
    # files. Refusal in one directory must not partially install into another.
    foreach ($directory in $directories) {
        foreach ($file in $prepared.files) {
            $path = Resolve-VictoryWarpWritePath (Join-Path $directory $file.name)
            $action = 'copied'
            if (Test-Path -LiteralPath $path) {
                if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "WARP destination is not a file: $path" }
                $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($actual -eq $file.sha256) { $action = 'reused' }
                elseif ($ExistingDllPolicy -eq 'ReplaceDifferent') { $action = 'replaced' }
                else { throw "Refusing to replace different WARP file $path. Select ExistingDllPolicy ReplaceDifferent explicitly to replace it." }
            }
            $pending.Add([pscustomobject]@{ directory = $directory; path = $path; source = $file.path; kind = $file.kind; sha256 = $file.sha256; action = $action })
        }
    }
    foreach ($file in $pending) {
        if ($file.action -eq 'reused') { continue }
        [void][IO.Directory]::CreateDirectory($file.directory)
        $temporary = Join-Path $file.directory ([Guid]::NewGuid().ToString('N') + '.warp-partial')
        try {
            [IO.File]::Copy($file.source, $temporary, $false)
            Assert-VictoryWarpHash $temporary $file.sha256
            # An atomic rename avoids editing a destination hard link in place.
            [IO.File]::Move($temporary, $file.path, ($file.action -eq 'replaced'))
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        }
    }
    $installations = [Collections.Generic.List[object]]::new()
    foreach ($directory in $directories) {
        $installation = [ordered]@{ directory = $directory }
        foreach ($file in @($pending | Where-Object directory -EQ $directory)) {
            Assert-VictoryWarpHash $file.path $file.sha256
            $installation[$file.kind + '_path'] = $file.path
            $installation[$file.kind + '_sha256'] = $file.sha256
            $installation[$file.kind + '_action'] = $file.action
        }
        $installation['dll_version'] = Get-VictoryWarpDllVersion $installation.dll_path
        $installations.Add([pscustomobject]$installation)
    }
    $result = [pscustomobject][ordered]@{
        schema_version = 1
        prepared_at_utc = [DateTime]::UtcNow.ToString('o')
        package_name = $prepared.package_name
        package_version = $prepared.package_version
        package_url = $prepared.package_url
        package_path = $prepared.package_path
        package_sha256 = $prepared.package_sha256
        package_sha512_base64 = $prepared.package_sha512_base64
        architecture = $prepared.architecture
        dll_version = $prepared.dll_version
        existing_dll_policy = $ExistingDllPolicy
        installations = $installations.ToArray()
        report_path = $ReportPath
    }
    if ($ReportPath) {
        [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($ReportPath))
        $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ReportPath -Encoding utf8
    }
    return $result
}

if ($MyInvocation.InvocationName -ne '.') {
    Initialize-VictoryWarp @PSBoundParameters
}
