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
    Write-Error "VSINSTALLDIR / VSTUDIO_ROOT not set — cannot find Visual Studio"
    exit 1
}
$vsDevCmd = Join-Path $vsRoot "Common7\Tools\VsDevCmd.bat"

# Run cmake inside a cmd session that sources VsDevCmd.bat first.
cmd /c "`"$vsDevCmd`" -arch=amd64 && cmake --preset=$Preset -B $BuildDir -DCMAKE_BUILD_TYPE=$BuildType && cmake --build $BuildDir -j"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& ".\$BuildDir\test\unittests\iis_injector_tests.exe"
