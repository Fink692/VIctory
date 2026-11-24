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
    [string]$WarpPackagePath,
    [switch]$RequireGpu,
    [switch]$NativeShaderDebug
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Test-Victory.ps1 requires Windows x64 and the MSVC build tools.' }
if ($NativeShaderDebug -and -not $RequireGpu) { throw '-NativeShaderDebug requires -RequireGpu.' }
. (Join-Path $PSScriptRoot 'Diagnostics.Common.ps1')
. (Join-Path $PSScriptRoot 'Initialize-VictoryWarp.ps1')
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
    native_shader_debug = [bool]$NativeShaderDebug
    formatting_base = $FormatBaseRef
    formatting_scope = 'Complete changed Rust files; full workspace when rustfmt configuration changes.'
    checks = $checks
    limitations = @('No game files are executed.', 'Console-SDK differential tests and ignored reference-data tests are not part of this gate.', 'WARP does not establish physical GPU compatibility or world rendering.', 'Optional atomic64 and depth-bounds checks may be unsupported; inspect each coverage result.')
}

function Invoke-Check {
    param([string]$Name, [string]$Executable, [string[]]$Arguments, [switch]$MustRunOneTest, [switch]$MustValidateAtomicMatrix, [switch]$OptionalAtomicSemantics, [switch]$MustValidateLdsMatrix, [switch]$MustExecuteLds, [switch]$OptionalDepthBounds, [switch]$MustMatchShaderHash, [switch]$MustValidateLdsDword, [switch]$MustExecuteLdsDword, [switch]$MustExecuteMemoryCopy, [switch]$MustExecuteWaterfall, [switch]$MustExecuteSampleOffsets, [switch]$MustExecuteDescriptorExhaustion, [uint32]$ConstantCases = 0, [uint32]$RectangleCases = 0, [hashtable]$EnvironmentOverrides = @{})
    Write-Host "Checking $Name..."
    $checkEnvironment = $environment.Clone()
    foreach ($key in $EnvironmentOverrides.Keys) { $checkEnvironment[$key] = $EnvironmentOverrides[$key] }
    $result = Invoke-VictoryProcess -Executable $Executable -Arguments $Arguments -WorkingDirectory $script:VictoryRepoRoot -LogPrefix (Join-Path $report $Name) -Environment $checkEnvironment -TimeoutSeconds 1200
    $check = [ordered]@{ name = $Name; process = $result; status = 'running' }
    if ($EnvironmentOverrides.Count -gt 0) { $check['environment_overrides'] = $EnvironmentOverrides }
    $checks.Add($check)
    try {
        Assert-VictoryProcessSucceeded $result $Name
        if ($MustRunOneTest -or $MustValidateAtomicMatrix -or $OptionalAtomicSemantics -or $MustValidateLdsMatrix -or $MustExecuteLds -or $OptionalDepthBounds -or $MustMatchShaderHash -or $MustValidateLdsDword -or $MustExecuteLdsDword -or $MustExecuteMemoryCopy -or $MustExecuteWaterfall -or $MustExecuteSampleOffsets -or $MustExecuteDescriptorExhaustion -or $ConstantCases -gt 0 -or $RectangleCases -gt 0) {
            $output = (Get-Content -LiteralPath $result.stdout_log -Raw) + (Get-Content -LiteralPath $result.stderr_log -Raw)
            if ($output -notmatch 'test result: ok\. 1 passed;' -or ($MustRunOneTest -and $output -match '; skipped')) {
                throw "$Name did not run its required test successfully. See $($result.stdout_log)."
            }
            if ($MustValidateAtomicMatrix) { $check['coverage'] = Get-VictoryAtomicValidationCoverage $output }
            if ($MustValidateLdsMatrix) { $check['coverage'] = Get-VictoryLdsValidationCoverage $output }
            if ($MustExecuteLds) { $check['coverage'] = Get-VictoryLdsCoverage $output }
            if ($MustMatchShaderHash) { $check['coverage'] = Get-VictoryShaderHashCoverage $output }
            if ($MustValidateLdsDword) { $check['coverage'] = Get-VictoryLdsDwordValidationCoverage $output }
            if ($MustExecuteLdsDword) { $check['coverage'] = Get-VictoryLdsDwordCoverage $output }
            if ($MustExecuteMemoryCopy) { $check['coverage'] = Get-VictoryMemoryCopyCoverage $output }
            if ($MustExecuteWaterfall) { $check['coverage'] = Get-VictoryWaterfallCoverage $output }
            if ($MustExecuteSampleOffsets) { $check['coverage'] = Get-VictorySampleOffsetCoverage $output }
            if ($MustExecuteDescriptorExhaustion) { $check['coverage'] = Get-VictoryDescriptorExhaustionCoverage $output }
            if ($ConstantCases -gt 0) { $check['coverage'] = Get-VictoryConstantCoverage $output $ConstantCases }
            if ($RectangleCases -gt 0) { $check['coverage'] = Get-VictoryRectangleCoverage $output $RectangleCases }
            if ($OptionalDepthBounds) {
                $check['coverage'] = Get-VictoryDepthBoundsCoverage $output
                Write-Host "$Name coverage: $($check.coverage.status); cases=$($check.coverage.cases)."
            }
            if ($OptionalAtomicSemantics) {
                $check['coverage'] = Get-VictoryAtomic64Coverage $output
                Write-Host "$Name coverage: $($check.coverage.status); adapters=$($check.coverage.adapters); cases=$($check.coverage.cases)."
            }
        }
        $check.status = 'passed'
    } catch {
        # Independent checks still provide useful evidence after a native
        # crash. Preserve every failure and reject the overall gate below.
        $check.status = 'failed'
        $check['error'] = $_.Exception.Message
        Write-Warning "$Name failed: $($check.error)"
    }
}

try {
    Invoke-Check 'toolchain' 'rustc' @('--version', '--verbose')
    Invoke-Check 'format' (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File', (Join-Path $PSScriptRoot 'Test-ChangedRustFormat.ps1'), '-BaseRef', $FormatBaseRef)
    $warp = Initialize-VictoryWarp -CacheDirectory (Join-Path $report 'warp-package') -ApplicationDirectory @((Join-Path $script:VictoryRepoRoot 'target/debug/deps')) -PackagePath $WarpPackagePath -ReportPath (Join-Path $report 'warp-runtime.json')
    $summary['warp_runtime'] = $warp
    $warpDll = $warp.installations[0].dll_path
    Write-Host "Prepared Microsoft WARP $($warp.dll_version) for synthetic tests."
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
        # Flushed phase markers localize native validator/driver crashes,
        # including a crash that occurs only in the later parallel suite.
        $environment.VICTORY_ATOMIC_TRACE = '1'
        $summary['dxil_validator'] = $validator
        $summary['atomic_phase_trace'] = $true
        Invoke-Check 'dxil-validator' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::tests::constant_color_validates', '--', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'atomic-dxil-validator' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::atomic_tests::image_atomics_translate_in_both_widths_and_return_modes', '--', '--exact', '--nocapture') -MustRunOneTest -MustValidateAtomicMatrix
        $compiler = Join-Path (Split-Path $validator -Parent) 'dxc.exe'
        Invoke-Check 'shader-hash-reference' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::container::tests::shader_hash_matches_official_dxc', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -MustMatchShaderHash -EnvironmentOverrides @{ VICTORY_DXC = $compiler }
        Invoke-Check 'texture-sample-offsets' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::image_tests::constant_sample_offsets_read_the_expected_texels', '--', '--exact', '--nocapture') -MustRunOneTest -MustExecuteSampleOffsets
        Invoke-Check 'waterfall-descriptors' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::waterfall_tests::waterfall_descriptor_selection_matches_each_lane', '--', '--exact', '--nocapture') -MustRunOneTest -MustExecuteWaterfall
        Invoke-Check 'warp-pipeline' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::tests::stub_pipeline_is_created', '--', '--exact', '--nocapture') -MustRunOneTest
        Invoke-Check 'atomic64-unguarded-semantics' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::atomic_tests::image_atomic64_results_match_integer_semantics', '--', '--exact', '--nocapture') -OptionalAtomicSemantics -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'atomic64-guarded-semantics' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::atomic_tests::guarded_image_atomic64_results_match_integer_semantics', '--', '--exact', '--nocapture') -OptionalAtomicSemantics -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        if ($NativeShaderDebug) {
            Invoke-Check 'native-shader-debug' (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File', (Join-Path $PSScriptRoot 'Test-NativeShader.ps1'), '-OutputRoot', $report, '-ExpectedWarpDll', $warpDll, '-ExpectedWarpVersion', $warp.dll_version)
            Invoke-Check 'native-gpu-debug' (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File', (Join-Path $PSScriptRoot 'Test-NativeGpu.ps1'), '-OutputRoot', $report, '-ExpectedWarpDll', $warpDll, '-ExpectedWarpVersion', $warp.dll_version)
        }
        Invoke-Check 'lds-dxil-validator' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::lds_tests::lds_subword_programs_translate_and_validate', '--', '--exact', '--nocapture') -MustRunOneTest -MustValidateLdsMatrix
        Invoke-Check 'lds-semantics' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::lds_tests::lds_subword_results_preserve_adjacent_bytes', '--', '--exact', '--nocapture') -MustRunOneTest -MustExecuteLds
        Invoke-Check 'lds-dword-dxil-validator' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::lds_dword_tests::lds_dword_programs_translate_and_validate', '--', '--exact', '--nocapture') -MustRunOneTest -MustValidateLdsDword
        Invoke-Check 'lds-dword-semantics' 'cargo' @('test', '--locked', '-p', 'victory-shader', '--all-features', 'emit::lds_dword_tests::lds_dword_bounds_and_inactive_lanes_preserve_memory', '--', '--exact', '--nocapture') -MustRunOneTest -MustExecuteLdsDword -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1'; VICTORY_LDS_TRACE = '1' }
        Invoke-Check 'warp-depth-bounds' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'pipeline::host::depth_bounds_tests::depth_bounds_filter_stored_depth_and_reset', '--', '--ignored', '--exact', '--nocapture') -OptionalDepthBounds -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-texture-copy-order' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'resource::copy_tests::gpu_texture_copies_preserve_order_and_guards', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-texture-copy-lists' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'resource::copy_tests::texture_readback_crosses_lists_and_reuses_subresources', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-texture-copy-ranges' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'resource::copy_tests::invalid_texture_copy_ranges_leave_destination_unchanged', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-texture-copy-bc' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'resource::copy_tests::block_compressed_small_mips_round_trip', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-texture-copy-planes' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'resource::copy_tests::depth_stencil_copies_use_selected_plane_formats', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-memory-copy' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'list::memory_copy_tests::raw_memory_copies_observe_gpu_writes_and_preserve_guards', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -MustExecuteMemoryCopy -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-constant-data' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'draw::constant_tests::constant_buffers_follow_gpu_writes_and_table_reuse', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -ConstantCases 24 -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-constant-lists' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'draw::constant_tests::constant_buffers_follow_submission_order_and_reexecute', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -ConstantCases 10 -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-descriptor-exhaustion' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'draw::constant_tests::exhausted_constant_tables_drop_dispatches_and_recover', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -MustExecuteDescriptorExhaustion -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
        Invoke-Check 'warp-rectangle-data' 'cargo' @('test', '--locked', '-p', 'victory-gpu', '--all-features', 'primitive::gpu::tests::gpu_rectangles_observe_copies_formats_and_rebind_draw_state', '--', '--ignored', '--exact', '--nocapture') -MustRunOneTest -RectangleCases 24 -EnvironmentOverrides @{ VICTORY_D3D12_DEBUG = '1' }
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
    $failed = @($checks | Where-Object { $_.status -eq 'failed' } | ForEach-Object { $_.name })
    $summary['failed_checks'] = $failed
    if ($failed.Count -gt 0) { throw "Validation failed: $($failed -join ', '). See each check's report for details." }
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
