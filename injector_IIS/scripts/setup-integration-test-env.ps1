# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

<#
.SYNOPSIS
    Set up the IIS integration test environment.

.DESCRIPTION
    Idempotent setup script that:
      1. Installs uv (Python package manager) if absent
      2. Installs IIS features, URL Rewrite, and ARR
      3. Removes any injector a previous run left, whichever way it was
         registered, then registers this one: either by installing the MSI
         (-MsiPath) or by staging the raw DLL (-DllPath)
      4. Creates test IIS sites with reverse proxy rules and RUM configuration

    CI runs both modes on the same reused Windows hosts, so neither may assume
    the host is clean: an MSI install fails on a module registered by hand, and
    an MSI uninstall fails on a module unregistered by hand.

    Run this once per machine (or after rebuilding the DLL/MSI).
    To run the tests use run-integration-tests.ps1.

.PARAMETER DllPath
    Path to the built iis_injector.dll. Mutually exclusive with -MsiPath.

.PARAMETER MsiPath
    Path to the built installer MSI. When provided the MSI is installed instead
    of manually staging the DLL. Mutually exclusive with -DllPath.

.PARAMETER RepoRoot
    Repository root. Defaults to two directories above this script.

.EXAMPLE
    # Raw DLL (after a local native build):
    .\setup-integration-test-env.ps1 -DllPath .\injector_IIS\build\iis_injector.dll

    # MSI (after building the installer):
    .\setup-integration-test-env.ps1 -MsiPath .\injector_IIS\build\msi\injector_IIS.msi

    # CI artifacts:
    .\setup-integration-test-env.ps1 -MsiPath .\artifacts\injector_IIS.msi -RepoRoot .
#>
param(
    [string]$DllPath,
    [string]$MsiPath,
    [string]$RepoRoot
)

#Requires -RunAsAdministrator
$ErrorActionPreference = "Stop"

if (-not $DllPath -and -not $MsiPath) {
    throw "Provide either -DllPath or -MsiPath."
}
if ($DllPath -and $MsiPath) {
    throw "-DllPath and -MsiPath are mutually exclusive."
}

$ScriptDir = Split-Path $MyInvocation.MyCommand.Path -Parent
if (-not $RepoRoot) {
    $RepoRoot = Split-Path (Split-Path $ScriptDir -Parent) -Parent
}

$RepoRoot = Resolve-Path $RepoRoot
if ($DllPath) { $DllPath = Resolve-Path $DllPath }
if ($MsiPath) { $MsiPath = Resolve-Path $MsiPath }

Write-Host "RepoRoot : $RepoRoot"
if ($DllPath) { Write-Host "DllPath  : $DllPath" }
if ($MsiPath) { Write-Host "MsiPath  : $MsiPath" }

# Pinned third-party artifacts. Update version + hash together when bumping.
$UvVersion           = "0.11.14"
$UvInstallerSha256   = "876568F96D76F8812002FAEC712E222AAF14ADAEF4FCC3FB8C44CDAAF826A1B7"
$UrlRewriteMsiSha256 = "37342FF2F585F263F34F48E9DE59EB1051D61015A8E967DBDE4075716230A32A"
$ArrMsiSha256        = "FB61FDB7101795A34D5129CB37EEE43AB675C7ED76BA3A3B23B039D8C90C2A4B"

function Assert-LastExitCode {
    if ($LASTEXITCODE -ne 0) { throw "Command failed with exit code $LASTEXITCODE" }
}

function Assert-FileSha256 {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$ExpectedHash,
        [Parameter(Mandatory)] [string]$Description
    )
    $actual = (Get-FileHash -Path $Path -Algorithm SHA256).Hash
    if ($actual -ne $ExpectedHash.ToUpperInvariant()) {
        throw "$Description SHA256 mismatch. Expected $ExpectedHash, got $actual (file: $Path)."
    }
}

# ---------------------------------------------------------------------------
# 1. Ensure uv is available (Python package manager used by the test runner)
# ---------------------------------------------------------------------------
function Ensure-Uv {
    if (Get-Command uv -ErrorAction SilentlyContinue) {
        Write-Host "uv already available."
        return
    }

    Write-Host "Installing uv $UvVersion..."
    $env:UV_INSTALL_DIR = "$env:ProgramFiles\uv"
    $installScript = Join-Path $env:TEMP "uv-install.ps1"
    Invoke-WebRequest -Uri "https://astral.sh/uv/$UvVersion/install.ps1" -OutFile $installScript -UseBasicParsing
    Assert-FileSha256 -Path $installScript -ExpectedHash $UvInstallerSha256 -Description "uv installer"
    & $installScript

    # Persist to the user PATH so subsequent processes (e.g. run-integration-tests.ps1)
    # find uv even though they are launched from a separate parent shell.
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $userPathEntries = if ($userPath) { $userPath -split ';' | Where-Object { $_ } } else { @() }
    if ($userPathEntries -notcontains $env:UV_INSTALL_DIR) {
        $newUserPath = if ($userPath) { "$env:UV_INSTALL_DIR;$userPath" } else { $env:UV_INSTALL_DIR }
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    }

    # Also add to PATH for the current process so subsequent commands find it.
    $env:PATH = "$env:UV_INSTALL_DIR;$env:PATH"

    if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
        throw "uv installation failed."
    }
    Write-Host "uv installed successfully."
}

# ---------------------------------------------------------------------------
# 2. Install IIS features, URL Rewrite, and ARR
# ---------------------------------------------------------------------------
function Install-IisPrerequisites {
    # IIS features (Windows Server)
    Write-Host "Enabling IIS Windows features..."
    $features = @(
        "Web-Server",
        "Web-Filtering",
        "Web-Http-Redirect",
        "Web-Mgmt-Console",
        "Web-Scripting-Tools"   # Installs the WebAdministration PowerShell module
    )
    $result = Install-WindowsFeature -Name $features -IncludeManagementTools
    if ($result.RestartNeeded -eq "Yes") {
        throw "A restart is needed to complete IIS installation. Please restart and re-run this script."
    }

    # URL Rewrite 2.1 -- check registry key written by the MSI installer
    $urlRewriteKey = "HKLM:\SOFTWARE\Microsoft\IIS Extensions\URL Rewrite"
    if (-not (Test-Path $urlRewriteKey)) {
        Write-Host "Installing URL Rewrite 2.1..."
        $msi = "$env:TEMP\urlrewrite.msi"
        Invoke-WebRequest -Uri "https://download.microsoft.com/download/1/2/8/128E2E22-C1B9-44A4-BE2A-5859ED1D4592/rewrite_amd64_en-US.msi" -OutFile $msi -UseBasicParsing
        Assert-FileSha256 -Path $msi -ExpectedHash $UrlRewriteMsiSha256 -Description "URL Rewrite MSI"
        $proc = Start-Process msiexec -ArgumentList "/i `"$msi`" /qn" -Wait -PassThru -NoNewWindow
        if ($proc.ExitCode -ne 0) { throw "URL Rewrite installation failed with exit code $($proc.ExitCode)" }
    } else {
        Write-Host "URL Rewrite already installed."
    }

    # Application Request Routing 3.0
    $arrKey = "HKLM:\SOFTWARE\Microsoft\IIS Extensions\Application Request Routing"
    if (-not (Test-Path $arrKey)) {
        Write-Host "Installing Application Request Routing 3.0..."
        $msi = "$env:TEMP\arr.msi"
        Invoke-WebRequest -Uri "https://download.microsoft.com/download/E/9/8/E9849D6A-020E-47E4-9FD0-A023E99B54EB/requestRouter_amd64.msi" -OutFile $msi -UseBasicParsing
        Assert-FileSha256 -Path $msi -ExpectedHash $ArrMsiSha256 -Description "Application Request Routing MSI"
        $proc = Start-Process msiexec -ArgumentList "/i `"$msi`" /qn" -Wait -PassThru -NoNewWindow
        if ($proc.ExitCode -ne 0) { throw "Application Request Routing installation failed with exit code $($proc.ExitCode)" }
    } else {
        Write-Host "Application Request Routing already installed."
    }
}

# ---------------------------------------------------------------------------
# 3. Remove any injector already on the host, MSI-installed or registered by hand
# ---------------------------------------------------------------------------
function Remove-Injector {
    $appcmd     = "$env:windir\System32\inetsrv\appcmd.exe"
    $moduleName = "DDRumModule"

    # The MSI's uninstall custom actions (appcmd uninstall module) need IIS
    # running, and a run that failed after stopping it leaves it stopped.
    iisreset /start | Out-Null
    Assert-LastExitCode

    $uninstallPaths = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*"
    )
    $installed = Get-ItemProperty $uninstallPaths -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -like 'Datadog RUM *' }
    foreach ($product in $installed) {
        Write-Host "Uninstalling existing Datadog RUM v$($product.DisplayVersion) ..."
        $unProc = Start-Process msiexec -ArgumentList "/x `"$($product.PSChildName)`" /qn IISRESTART=true" -Wait -PassThru -NoNewWindow
        if ($unProc.ExitCode -ne 0) {
            throw "MSI uninstall of Datadog RUM v$($product.DisplayVersion) failed with exit code $($unProc.ExitCode)"
        }
        Write-Host "Uninstalled."
    }

    # Left registered by hand (-DllPath), which no MSI uninstall removes.
    $existing = & $appcmd list module "/name:$moduleName" 2>&1
    if ($LASTEXITCODE -eq 0 -and $existing) {
        Write-Host "Unregistering module '$moduleName' left by a previous run..."
        & $appcmd uninstall module $moduleName
        Assert-LastExitCode
    }
}

# ---------------------------------------------------------------------------
# 4. Register the IIS module (replaces what the MSI does, no installer needed)
# ---------------------------------------------------------------------------
function Register-IisModule {
    $schemaDir  = "$env:windir\System32\inetsrv\config\schema"
    $schemaFile = "$RepoRoot\injector_IIS\resources\RUM_schema.xml"
    $appcmd     = "$env:windir\System32\inetsrv\appcmd.exe"
    $configFile = "$env:windir\System32\inetsrv\config\applicationHost.config"
    $moduleName = "DDRumModule"

    # 3a. Copy schema
    Write-Host "Copying RUM schema..."
    Copy-Item $schemaFile $schemaDir -Force

    # 3b. Add config section declarations to applicationHost.config
    # Stop IIS so the config file is not held by another process
    Write-Host "Stopping IIS to allow config edits..."
    iisreset /stop | Out-Null
    Assert-LastExitCode

    Write-Host "Registering config sections in applicationHost.config..."
    $xml = [xml](Get-Content $configFile -Raw)

    $groups = $xml.configuration.configSections.sectionGroup

    # system.webServer/datadogRum (overrideModeDefault=Allow so web.config can override)
    $webServerGroup = $groups | Where-Object { $_.name -eq "system.webServer" }
    if (-not ($webServerGroup.section | Where-Object { $_.name -eq "datadogRum" })) {
        $sec = $xml.CreateElement("section")
        $sec.SetAttribute("name", "datadogRum")
        $sec.SetAttribute("overrideModeDefault", "Allow")
        $webServerGroup.AppendChild($sec) | Out-Null
    }

    # system.applicationHost/datadogRumServer (AppHostOnly -- not delegatable)
    $appHostGroup = $groups | Where-Object { $_.name -eq "system.applicationHost" }
    if (-not ($appHostGroup.section | Where-Object { $_.name -eq "datadogRumServer" })) {
        $sec = $xml.CreateElement("section")
        $sec.SetAttribute("name", "datadogRumServer")
        $sec.SetAttribute("allowDefinition", "AppHostOnly")
        $sec.SetAttribute("overrideModeDefault", "Deny")
        $appHostGroup.AppendChild($sec) | Out-Null
    }

    $xml.Save($configFile)

    # 3c. Copy DLL to a system directory that IIS worker processes can read.
    # ApplicationPoolIdentity accounts cannot access paths under C:\Users\,
    # so we stage the DLL under C:\inetpub where IIS_IUSRS already has access.
    $stageDir = "C:\inetpub\DDRum"
    $stagedDll = "$stageDir\iis_injector.dll"
    Write-Host "Staging DLL to $stagedDll ..."
    New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
    Copy-Item $DllPath $stagedDll -Force

    Write-Host "Registering IIS module '$moduleName'..."
    & $appcmd install module "/name:$moduleName" "/image:$stagedDll" /precondition:bitness64
    Assert-LastExitCode
}

# ---------------------------------------------------------------------------
# 5. Create test IIS sites
# ---------------------------------------------------------------------------
function Add-RewriteProxyRule {
    param([string]$SiteName)

    Import-Module WebAdministration -ErrorAction Stop

    $ruleName = "Proxy-to-upstream"
    $pspath   = "MACHINE/WEBROOT/APPHOST/$SiteName"
    $root     = "system.webServer/rewrite/rules"
    $filter   = "$root/rule[@name='$ruleName']"

    if (Get-WebConfiguration -Filter $filter -PSPath $pspath -ErrorAction SilentlyContinue) {
        Write-Host "  Rewrite rule already present on '$SiteName'."
        return
    }

    Add-WebConfigurationProperty -PSPath $pspath -filter $root -name "." `
        -value @{name = $ruleName; patternSyntax = "Regular Expressions"; enabled = "True"; stopProcessing = "True"}
    Set-WebConfigurationProperty -PSPath $pspath -filter "$filter/match" -name "url" -value "(.*)"
    Set-WebConfigurationProperty -PSPath $pspath -filter "$filter/conditions" -name "." `
        -value @{input = "{REQUEST_FILENAME}"; matchType = "IsDirectory"; negate = "true"},
               @{input = "{REQUEST_FILENAME}"; matchType = "IsFile"; negate = "true"}
    Set-WebConfigurationProperty -PSPath $pspath -filter "$filter/action" -name "type" -value "Rewrite"
    Set-WebConfigurationProperty -PSPath $pspath -filter "$filter/action" -name "url" -value "http://localhost:8090/{R:1}"
}

function New-TestSites {
    Import-Module WebAdministration -ErrorAction Stop
    Import-Module "$RepoRoot\injector_IIS\resources\modules\Datadog.RUM.psm1" -Force

    $staticSrc  = "$RepoRoot\tests\integration_tests\static"
    $appPool    = "DDRumTestPool"

    # Dedicated app pool so the test sites are isolated from DefaultAppPool
    if (-not (Get-WebConfigurationProperty -Filter "system.applicationHost/applicationPools/add[@name='$appPool']" `
              -PSPath "MACHINE/WEBROOT/APPHOST" -Name "name" -ErrorAction SilentlyContinue)) {
        Write-Host "Creating app pool '$appPool'..."
        New-WebAppPool -Name $appPool
    } else {
        Write-Host "App pool '$appPool' already exists."
    }

    # Enable ARR reverse proxy globally
    Set-WebConfigurationProperty -PSPath "MACHINE/WEBROOT/APPHOST" `
        -filter "system.webServer/proxy" -name "enabled" -value "True"

    # Site 1: RUM enabled (port 8080)
    $site1Name = "TestRumEnabled"
    $site1Root = "$env:SystemDrive\$site1Name"

    if (-not (Get-Website -Name $site1Name -ErrorAction SilentlyContinue)) {
        Write-Host "Creating site '$site1Name'..."
        New-Item -ItemType Directory -Force -Path $site1Root | Out-Null
        if (Test-Path $staticSrc) {
            Copy-Item "$staticSrc\*" $site1Root -Recurse -Force
        }
        New-Website -Name $site1Name -Port 8080 -PhysicalPath $site1Root -ApplicationPool $appPool -Force
    } else {
        Write-Host "Site '$site1Name' already exists."
        Set-ItemProperty "IIS:\Sites\$site1Name" -Name applicationPool -Value $appPool
    }

    Add-RewriteProxyRule -SiteName $site1Name

    New-Datadog-RUMConfiguration `
        -IISSite              $site1Name `
        -ApplicationId        "foo" `
        -ClientToken          "bar" `
        -DatadogSite          "datadoghq.eu" `
        -SessionSampleRate       100 `
        -SessionReplaySampleRate 100

    # Site 2: RUM disabled/absent (port 8081) -- used for negative tests
    $site2Name = "TestRumDisabled"
    $site2Root = "$env:SystemDrive\$site2Name"

    if (-not (Get-Website -Name $site2Name -ErrorAction SilentlyContinue)) {
        Write-Host "Creating site '$site2Name'..."
        New-Item -ItemType Directory -Force -Path $site2Root | Out-Null
        if (Test-Path $staticSrc) {
            Copy-Item "$staticSrc\*" $site2Root -Recurse -Force
        }
        New-Website -Name $site2Name -Port 8081 -PhysicalPath $site2Root -ApplicationPool $appPool -Force
    } else {
        Write-Host "Site '$site2Name' already exists."
        Set-ItemProperty "IIS:\Sites\$site2Name" -Name applicationPool -Value $appPool
    }

    Add-RewriteProxyRule -SiteName $site2Name
    # Intentionally no RUM configuration on this site

    Write-Host "Restarting IIS to apply all changes..."
    iisreset /restart
    Assert-LastExitCode
}

# ---------------------------------------------------------------------------
# 6. Install the module via MSI (alternative to Register-IisModule)
# ---------------------------------------------------------------------------
function Install-InjectorMsi {
    Write-Host "Installing MSI: $MsiPath ..."
    $logFile = "$env:TEMP\injector_IIS_install.log"
    # IISRESTART=true is required by the MSI for silent installs (acknowledges
    # that IIS will be restarted). IIS was already stopped above.
    $proc = Start-Process msiexec -ArgumentList "/i `"$MsiPath`" /qn IISRESTART=true /l*v `"$logFile`"" -Wait -PassThru -NoNewWindow
    if ($proc.ExitCode -ne 0) {
        Write-Host "MSI install log:" (Get-Content $logFile -Raw -ErrorAction SilentlyContinue)
        throw "MSI installation failed with exit code $($proc.ExitCode)"
    }
    Write-Host "MSI installed successfully."
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
Ensure-Uv
Install-IisPrerequisites

Remove-Injector

if ($MsiPath) {
    # MSI mode: the installer handles DLL staging, schema, appcmd, and
    # applicationHost.config section declarations.
    Write-Host "Stopping IIS before MSI installation..."
    iisreset /stop | Out-Null
    Assert-LastExitCode
    Install-InjectorMsi
} else {
    # Manual mode: stage DLL and register module by hand.
    Register-IisModule
}

New-TestSites

Write-Host ""
Write-Host "Setup complete. Run .\injector_IIS\scripts\run-integration-tests.ps1 to execute the tests."
