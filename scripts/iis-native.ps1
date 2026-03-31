# iis-native.ps1 <configure|build|test|shell>
#
# Sets up MSVC + Rust environment and runs IIS injector build tasks natively.
# Requires: VS 2022 Build Tools, Rust (rustup)
param(
    [Parameter(Position=0)]
    [ValidateSet('configure','build','test','shell','install','uninstall')]
    [string]$Target = 'build'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Locate Visual Studio via vswhere ---
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    Write-Error "vswhere.exe not found. Install VS 2022 Build Tools."
    exit 1
}

$vsPath = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if (-not $vsPath) {
    Write-Error "No VS installation with VC tools found."
    exit 1
}

# Add VS Installer dir to PATH so VsDevCmd.bat can find vswhere internally
$env:PATH += ";${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer"

# --- Activate VS dev environment via Invoke-VsDevShell ---
$devShell = "$vsPath\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
if (-not (Test-Path $devShell)) {
    Write-Error "DevShell DLL not found at: $devShell"
    exit 1
}

Import-Module $devShell
Enter-VsDevShell -VsInstallPath $vsPath -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'

# --- Verify MSVC ---
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    Write-Error "cl.exe not in PATH after VS env setup."
    exit 1
}

# --- Verify Rust ---
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found. Install Rust: winget install Rustlang.Rustup"
    exit 1
}

# --- Paths ---
$repoDir   = Split-Path $PSScriptRoot -Parent
$sourceDir = Join-Path $repoDir 'injector_IIS'
$buildDir  = Join-Path $repoDir 'injector_IIS\build'

Write-Host ""
Write-Host "VS:     $vsPath"
Write-Host "Repo:   $repoDir"
Write-Host "Build:  $buildDir"
Write-Host "cl:     $(Get-Command cl.exe | Select-Object -ExpandProperty Source)"
Write-Host "cargo:  $(cargo --version)"
Write-Host ""

switch ($Target) {
    'configure' {
        Write-Host "=== Configuring ==="
        cmake --preset=ci-native -B $buildDir -DCMAKE_BUILD_TYPE=Debug "-DCMAKE_POLICY_VERSION_MINIMUM=3.5" $sourceDir
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    'build' {
        Write-Host "=== Configuring + Building ==="
        cmake --preset=ci-native -B $buildDir -DCMAKE_BUILD_TYPE=Debug "-DCMAKE_POLICY_VERSION_MINIMUM=3.5" $sourceDir
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        cmake --build $buildDir -j
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    'test' {
        Write-Host "=== Running unit tests ==="
        & "$buildDir\test\unittests\iis_injector_tests.exe"
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    'shell' {
        Write-Host "=== Dropping into VS Developer PowerShell ==="
        powershell.exe -NoExit
    }
    'install' {
        Write-Host "=== Installing IIS injector module ==="

        $installDir  = "C:\Program Files\Datadog\Datadog RUM"
        $schemaDir   = "C:\Windows\System32\inetsrv\config\schema"
        $appcmd      = "C:\Windows\System32\inetsrv\appcmd.exe"
        $appHostConf = "C:\Windows\System32\inetsrv\config\applicationHost.config"
        $dllSrc      = "$buildDir\iis_injector.dll"
        $dllDst      = "$installDir\injector_IIS.dll"

        if (-not (Test-Path $dllSrc)) {
            Write-Error "DLL not found at $dllSrc - run 'build' first."
            exit 1
        }

        # 1. Copy DLL
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        Copy-Item $dllSrc $dllDst -Force
        Write-Host "  Copied DLL -> $dllDst"

        # 2. Copy schema
        Copy-Item "$repoDir\injector_IIS\resources\RUM_schema.xml" "$schemaDir\RUM_schema.xml" -Force
        Write-Host "  Copied schema -> $schemaDir\RUM_schema.xml"

        # 3. Add config sections to applicationHost.config (idempotent)
        [xml]$cfg = Get-Content $appHostConf

        $ensureSection = {
            param($sectionGroupName, $sectionName, $extraAttribs)
            $sg = $cfg.configuration.configSections.sectionGroup | Where-Object { $_.name -eq $sectionGroupName }
            if ($null -eq $sg) { Write-Warning "sectionGroup '$sectionGroupName' not found in applicationHost.config"; return }
            $existing = $sg.section | Where-Object { $_.name -eq $sectionName }
            if ($null -ne $existing) { Write-Host "  Section '$sectionName' already registered"; return }
            $el = $cfg.CreateElement('section')
            $el.SetAttribute('name', $sectionName)
            foreach ($kv in $extraAttribs.GetEnumerator()) { $el.SetAttribute($kv.Key, $kv.Value) }
            $sg.AppendChild($el) | Out-Null
            Write-Host "  Added config section '$sectionName' to $sectionGroupName"
        }

        & $ensureSection 'system.webServer'       'datadogRum'       @{ overrideModeDefault = 'Allow' }
        & $ensureSection 'system.applicationHost' 'datadogRumServer' @{ allowDefinition = 'AppHostOnly'; overrideModeDefault = 'Deny' }
        $cfg.Save($appHostConf)

        # 4. Register module with IIS
        $registered = & $appcmd list module DDRumModule 2>&1
        if ($registered -match 'MODULE "DDRumModule"') {
            Write-Host "  Module DDRumModule already registered"
        } else {
            & $appcmd install module /name:DDRumModule /image:"`"$dllDst`"" /precondition:bitness64
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            Write-Host "  Registered DDRumModule with IIS"
        }

        Write-Host ""
        Write-Host "Done. DLL installed and registered with IIS."
        Write-Host "Configure RUM via: C:\Windows\System32\inetsrv\config\applicationHost.config"
        Write-Host "  or per-site web.config using the datadogRum section."
    }
    'uninstall' {
        Write-Host "=== Uninstalling IIS injector module ==="

        $installDir = "C:\Program Files\Datadog\Datadog RUM"
        $appcmd     = "C:\Windows\System32\inetsrv\appcmd.exe"

        $registered = & $appcmd list module DDRumModule 2>&1
        if ($registered -match 'MODULE "DDRumModule"') {
            & $appcmd uninstall module DDRumModule
            Write-Host "  Unregistered DDRumModule"
        } else {
            Write-Host "  DDRumModule not registered, skipping"
        }

        if (Test-Path $installDir) {
            Remove-Item $installDir -Recurse -Force
            Write-Host "  Removed $installDir"
        }

        Write-Host "Done."
    }
}
