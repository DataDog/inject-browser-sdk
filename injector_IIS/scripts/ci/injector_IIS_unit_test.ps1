param(
    [string]$Preset = "ci-native",
    [string]$BuildDir = "build",
    [string]$BuildType = "Release"
)

$ErrorActionPreference = "Stop"

Set-Location $PSScriptRoot\..\..

# Initialize MSVC environment via VsDevCmd.bat (same approach as the build task).
# The Windows build image sets VSINSTALLDIR; fall back to VSTUDIO_ROOT.
$vsRoot = $env:VSINSTALLDIR
if (-not $vsRoot) { $vsRoot = $env:VSTUDIO_ROOT }
if (-not $vsRoot) {
    Write-Error "VSINSTALLDIR / VSTUDIO_ROOT not set - cannot find Visual Studio"
    exit 1
}
$vsDevCmd = Join-Path $vsRoot "Common7\Tools\VsDevCmd.bat"

# Run cmake inside a cmd session that sources VsDevCmd.bat first
# (matches the approach used by the invoke build task in iis.py).
# Use single-quoted here-string to avoid PowerShell 5.1 parsing && as pipeline chain.
$configureCmd = 'call "' + $vsDevCmd + '" -arch=amd64 && cmake --preset=' + $Preset + ' -B ' + $BuildDir + ' -DCMAKE_BUILD_TYPE=' + $BuildType
cmd /c $configureCmd
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$buildCmd = 'call "' + $vsDevCmd + '" -arch=amd64 && cmake --build ' + $BuildDir + ' -j'
cmd /c $buildCmd
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$testExe = Join-Path $BuildDir "test\unittests\iis_injector_tests.exe"
& $testExe
exit $LASTEXITCODE
