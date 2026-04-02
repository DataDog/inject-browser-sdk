param(
    [string]$Preset = "ci-native",
    [string]$BuildDir = "build",
    [string]$BuildType = "Release"
)

$ErrorActionPreference = "Stop"

Set-Location $PSScriptRoot\..

cmake --preset=$Preset -B $BuildDir -DCMAKE_BUILD_TYPE=$BuildType
cmake --build $BuildDir -j
& ".\$BuildDir\test\unittests\iis_injector_tests.exe"
