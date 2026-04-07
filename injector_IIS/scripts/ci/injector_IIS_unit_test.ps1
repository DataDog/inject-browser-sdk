param(
    [string]$Preset = "ci-native",
    [string]$BuildDir = "build",
    [string]$BuildType = "Release"
)

$ErrorActionPreference = "Stop"

Set-Location $PSScriptRoot\..\..

# Initialize MSVC environment (cl.exe, link.exe, etc.)
$vsPath = & vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    Write-Error "Visual Studio with C++ tools not found"
    exit 1
}
Import-Module "$vsPath\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vsPath -SkipAutomaticLocation -DevCmdArguments "-arch=amd64"

cmake --preset=$Preset -B $BuildDir -DCMAKE_BUILD_TYPE=$BuildType
cmake --build $BuildDir -j
& ".\$BuildDir\test\unittests\iis_injector_tests.exe"
