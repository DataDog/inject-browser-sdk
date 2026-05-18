# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

<#
.SYNOPSIS
    Build the IIS injector DLL and MSI installer locally.

.DESCRIPTION
    Locates Visual Studio and WiX 3.x, configures the MSVC environment, builds
    the C++ DLL with CMake, then builds the MSI with MSBuild.

    Output: injector_IIS\build\msi\injector_IIS_installer.msi

.PARAMETER MajVer
    Major version number (default: 0).

.PARAMETER MinVer
    Minor version number (default: 1).

.PARAMETER BuildVer
    Build number (default: 0). In CI this is the GitLab CI_PIPELINE_IID.

.EXAMPLE
    .\build-msi.ps1
    .\build-msi.ps1 -MajVer 1 -MinVer 3 -BuildVer 2100
#>
param(
    [int]$MajVer   = 0,
    [int]$MinVer   = 1,
    [int]$BuildVer = 0
)

$ErrorActionPreference = "Stop"

$ScriptDir   = Split-Path $MyInvocation.MyCommand.Path -Parent
$InjectorDir = Split-Path $ScriptDir -Parent

# Required by corrosion (Rust CMake integration)
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"

# -- Locate Visual Studio --
$vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    throw "vswhere.exe not found at '$vswhere'. Visual Studio must be installed."
}

$vsPath = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath

if (-not $vsPath) {
    throw "No Visual Studio installation with C++ tools found."
}

$vsInstanceId = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property instanceId

$devShellModule = "$vsPath\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
if (-not (Test-Path $devShellModule)) {
    throw "DevShell module not found at '$devShellModule'."
}

Write-Host "Using Visual Studio at: $vsPath"
Import-Module $devShellModule
Enter-VsDevShell -VsInstanceId $vsInstanceId -SkipAutomaticLocation -DevCmdArguments "-arch=amd64"

# -- Locate WiX 3.x --
$wixDir = "${env:ProgramFiles(x86)}\WiX Toolset v3.14"
if (-not (Test-Path $wixDir)) {
    $wixDir = "${env:ProgramFiles(x86)}\WiX Toolset v3.11"
}
if (-not (Test-Path $wixDir)) {
    throw "WiX Toolset 3.x not found. Install it via: winget install WiXToolset.WiXToolset"
}
Write-Host "Using WiX at: $wixDir"

Push-Location $InjectorDir
try {
    # -- Build DLL --
    Write-Host "Configuring with preset 'ci-native'..."
    cmake --preset=ci-native -B build -DCMAKE_BUILD_TYPE=RelWithDebInfo .
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "Building DLL..."
    cmake --build build --target iis_injector -j
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $dllPath = Resolve-Path ".\build\iis_injector.dll"
    Write-Host "DLL built: $dllPath"

    # -- Build MSI --
    $msiOutDir = "build\msi"
    New-Item -ItemType Directory -Force -Path $msiOutDir | Out-Null

    # WiX MSBuild targets are installed under the MSBuild extensions path,
    # not under the WiX toolset directory itself.
    $wixTargets = "${env:ProgramFiles(x86)}\MSBuild\Microsoft\WiX\v3.x\wix.targets"
    if (-not (Test-Path $wixTargets)) {
        throw "WiX MSBuild targets not found at '$wixTargets'. Ensure WiX 3.x is installed."
    }
    $wixproj    = ".\injector_IIS_installer\injector_IIS_installer.wixproj"

    Write-Host "Building MSI (v$MajVer.$MinVer.$BuildVer)..."
    msbuild $wixproj `
        /p:Configuration=Release `
        /p:Platform=x64 `
        /p:OutputPath="$((Resolve-Path '.').Path)\$msiOutDir\" `
        /p:IntermediateOutputPath="$((Resolve-Path '.').Path)\build\msi_obj\" `
        /p:InjectorDll="$dllPath" `
        /p:MAJ_VER=$MajVer `
        /p:MIN_VER=$MinVer `
        /p:BUILD_VER=$BuildVer `
        /p:WixTargetsPath="$wixTargets" `
        /nologo /v:minimal
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $msi = Get-ChildItem $msiOutDir -Filter "*.msi" -Recurse | Select-Object -First 1
    Write-Host ""
    Write-Host "MSI built: $($msi.FullName)"
    Write-Host "Run setup:  .\injector_IIS\scripts\setup-integration-test-env.ps1 -MsiPath '$($msi.FullName)'"
} finally {
    Pop-Location
}
