#Requires -Version 5.1

<#
.SYNOPSIS
  Datadog RUM IIS injector diagnostic flare collector.

.DESCRIPTION
  Read-only collector. Gathers IIS configuration, applicationHost.config,
  per-site web.configs, RUM injector binary metadata, registry, and
  Datadog-RUM-Instrumentation event log entries into a single output
  directory you can attach to a support ticket.

  - Writes nothing outside the chosen output directory.
  - Does not change IIS state, services, registry, environment, or files.
  - Does not require the WebAdministration / IISAdministration / Datadog.RUM
    PowerShell modules. Falls back to direct config file reads and
    appcmd.exe when modules are unavailable.
  - Continues on errors; failures are recorded in manifest.json.

.PARAMETER OutputDir
  Output directory. Default: ./flare under the working directory.
  If a directory at that path already exists, a timestamp suffix is appended
  so the script never deletes anything outside what it creates.

.PARAMETER MaxEvents
  Maximum events to fetch from the Datadog-RUM-Instrumentation provider.
  Default 2000.

.PARAMETER NoWebConfigs
  Skip walking site physical paths for web.config files.

.PARAMETER NoEventLogs
  Skip event-log collection.

.PARAMETER Zip
  After collection, compress the flare directory to <flare>.zip alongside it.

.EXAMPLE
  .\rum-flare.ps1

.EXAMPLE
  .\rum-flare.ps1 -OutputDir C:\Temp\rum-flare -Zip
#>

[CmdletBinding()]
param(
  [string]$OutputDir = (Join-Path (Get-Location) 'flare'),
  [int]$MaxEvents = 2000,
  [switch]$NoWebConfigs,
  [switch]$NoEventLogs,
  [switch]$Zip
)

$ErrorActionPreference = 'Continue'
$ProgressPreference    = 'SilentlyContinue'
$VerbosePreference     = 'SilentlyContinue'

# ============================================================
# helpers
# ============================================================

$script:Manifest = [ordered]@{
  schemaVersion = 1
  startedUtc    = (Get-Date).ToUniversalTime().ToString('o')
  finishedUtc   = $null
  outputDir     = $null
  isAdmin       = $null
  steps         = @()
}

function Write-Heading($t) { Write-Host "[+] $t" -ForegroundColor Cyan }
function Write-Skip($t)    { Write-Host "[-] $t" -ForegroundColor Yellow }
function Write-Fail($t)    { Write-Host "[!] $t" -ForegroundColor Red }

function Invoke-Step {
  param(
    [Parameter(Mandatory)] [string]$Label,
    [Parameter(Mandatory)] [scriptblock]$Body
  )
  Write-Heading $Label
  $entry = [ordered]@{
    label     = $Label
    ok        = $false
    error     = $null
    elapsedMs = 0
  }
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  try {
    & $Body
    $entry.ok = $true
  } catch {
    $entry.error = $_.Exception.Message
    Write-Fail "  $($_.Exception.Message)"
  }
  $sw.Stop()
  $entry.elapsedMs = [int]$sw.ElapsedMilliseconds
  $script:Manifest.steps += [pscustomobject]$entry
}

function Save-Json {
  param([string]$Path, $Object)
  ($Object | ConvertTo-Json -Depth 10) | Set-Content -LiteralPath $Path -Encoding UTF8
}

function Save-Text {
  param([string]$Path, [string]$Text)
  if ($null -eq $Text) { $Text = '' }
  Set-Content -LiteralPath $Path -Value $Text -Encoding UTF8
}

function Test-IsAdmin {
  ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-Appcmd {
  param([string[]]$ArgList, [string]$OutFile)
  $appcmd = "$env:SystemRoot\System32\inetsrv\appcmd.exe"
  if (-not (Test-Path -LiteralPath $appcmd)) {
    "appcmd.exe not found at $appcmd" | Set-Content -LiteralPath $OutFile -Encoding UTF8
    return $false
  }
  & $appcmd @ArgList 2>&1 | Out-File -LiteralPath $OutFile -Encoding UTF8
  return $true
}

function ConvertTo-SafeFileName {
  param([string]$Name)
  $s = ($Name -replace '[\\/:*?"<>|]', '_').Trim('_', ' ', '/')
  if (-not $s) { $s = 'unnamed' }
  return $s
}

# ============================================================
# 1. setup output dir (never wipes existing dirs)
# ============================================================

if (Test-Path -LiteralPath $OutputDir) {
  $ts = Get-Date -Format 'yyyyMMddTHHmmss'
  $OutputDir = "$OutputDir-$ts"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
foreach ($sub in @('appcmd','webconfigs','binaries','registry','events','network','schema','isolation')) {
  New-Item -ItemType Directory -Force -Path (Join-Path $OutputDir $sub) | Out-Null
}

$script:Manifest.outputDir = (Resolve-Path -LiteralPath $OutputDir).Path
$isAdmin = Test-IsAdmin
$script:Manifest.isAdmin = $isAdmin

Write-Host "Datadog RUM IIS flare collector" -ForegroundColor Green
Write-Host "  output: $($script:Manifest.outputDir)"
Write-Host "  admin : $isAdmin"
if (-not $isAdmin) {
  Write-Skip "Not running elevated. appcmd, registry, and some file reads may be partial."
}
Write-Host ""

# ============================================================
# 2. system info
# ============================================================

Invoke-Step 'system info' {
  $os = $null
  try { $os = Get-CimInstance Win32_OperatingSystem -ErrorAction Stop } catch {}
  $info = [ordered]@{
    timestamp_utc      = (Get-Date).ToUniversalTime().ToString('o')
    hostname           = $env:COMPUTERNAME
    domain             = $env:USERDOMAIN
    user               = "$env:USERDOMAIN\$env:USERNAME"
    is_admin           = $isAdmin
    powershell_version = $PSVersionTable.PSVersion.ToString()
    powershell_edition = "$($PSVersionTable.PSEdition)"
    os_caption         = if ($os) { $os.Caption } else { $null }
    os_version         = if ($os) { $os.Version } else { $null }
    os_build           = if ($os) { $os.BuildNumber } else { $null }
    architecture       = $env:PROCESSOR_ARCHITECTURE
    culture            = (Get-Culture).Name
    timezone           = [TimeZoneInfo]::Local.Id
  }
  Save-Json -Path (Join-Path $OutputDir 'system.json') -Object $info
}

# ============================================================
# 3. detection: IIS, modules, datadog
# ============================================================

Invoke-Step 'iis & datadog presence' {
  $appcmd     = "$env:SystemRoot\System32\inetsrv\appcmd.exe"
  $appHost    = "$env:SystemRoot\System32\inetsrv\config\applicationHost.config"
  $ddDll      = "$env:ProgramFiles\Datadog\Datadog RUM\injector_IIS.dll"
  $ddSchema   = "$env:SystemRoot\System32\inetsrv\config\schema\RUM_schema.xml"

  $modulesAvailable = @{
    WebAdministration = ($null -ne (Get-Module -ListAvailable -Name WebAdministration -ErrorAction SilentlyContinue))
    IISAdministration = ($null -ne (Get-Module -ListAvailable -Name IISAdministration -ErrorAction SilentlyContinue))
    'Datadog.RUM'     = ($null -ne (Get-Module -ListAvailable -Name 'Datadog.RUM'     -ErrorAction SilentlyContinue))
  }

  $svcW3svc = Get-Service -Name W3SVC -ErrorAction SilentlyContinue
  $svcWAS   = Get-Service -Name WAS   -ErrorAction SilentlyContinue
  $w3wp     = Get-Process -Name w3wp  -ErrorAction SilentlyContinue

  $info = [ordered]@{
    appcmd_present       = (Test-Path -LiteralPath $appcmd)
    appcmd_path          = $appcmd
    apphost_present      = (Test-Path -LiteralPath $appHost)
    apphost_path         = $appHost
    rum_dll_present      = (Test-Path -LiteralPath $ddDll)
    rum_dll_path         = $ddDll
    rum_schema_present   = (Test-Path -LiteralPath $ddSchema)
    rum_schema_path      = $ddSchema
    ps_modules_available = $modulesAvailable
    w3svc_status         = if ($svcW3svc) { $svcW3svc.Status.ToString() } else { 'not installed' }
    was_status           = if ($svcWAS)   { $svcWAS.Status.ToString()   } else { 'not installed' }
    w3wp_count           = if ($w3wp) { @($w3wp).Count } else { 0 }
    # NOTE: PS 5.1 ConvertTo-Json serialises empty arrays as `{}` — the
    # value here is correct either way; the JSON shape may render as `[]`
    # or `{}` depending on PS version. Reader should treat both as "no pids".
    w3wp_pids            = @(if ($w3wp) { $w3wp.Id } else { @() })
  }
  Save-Json -Path (Join-Path $OutputDir 'detect.json') -Object $info
}

# ============================================================
# 4. applicationHost.config (raw + extracted lines)
# ============================================================

Invoke-Step 'applicationHost.config' {
  $appHost = "$env:SystemRoot\System32\inetsrv\config\applicationHost.config"
  if (Test-Path -LiteralPath $appHost) {
    Copy-Item -LiteralPath $appHost -Destination (Join-Path $OutputDir 'applicationHost.config') -Force

    $hits = Select-String -LiteralPath $appHost -Pattern 'datadog|DDRum|injector_IIS' -CaseSensitive:$false
    $text = ($hits | ForEach-Object { '{0,5}: {1}' -f $_.LineNumber, $_.Line.Trim() } | Out-String)
    Save-Text -Path (Join-Path $OutputDir 'apphost-datadog-lines.txt') -Text $text
  } else {
    Save-Text -Path (Join-Path $OutputDir 'applicationHost.config.MISSING') `
              -Text "applicationHost.config not present at $appHost"
  }

  $schema = "$env:SystemRoot\System32\inetsrv\config\schema\RUM_schema.xml"
  if (Test-Path -LiteralPath $schema) {
    Copy-Item -LiteralPath $schema -Destination (Join-Path $OutputDir 'schema/RUM_schema.xml') -Force
  }
}

# ============================================================
# 5. appcmd dumps
# ============================================================

Invoke-Step 'appcmd dumps' {
  $appcmd = "$env:SystemRoot\System32\inetsrv\appcmd.exe"
  if (-not (Test-Path -LiteralPath $appcmd)) {
    Save-Text -Path (Join-Path $OutputDir 'appcmd/_NOT_FOUND.txt') -Text "$appcmd not found"
    return
  }
  $base = Join-Path $OutputDir 'appcmd'
  $cmds = @(
    @{ args = @('list','sites','/xml');     out = 'sites.xml' }
    @{ args = @('list','apps','/xml');      out = 'apps.xml' }
    @{ args = @('list','apppools','/xml');  out = 'apppools.xml' }
    @{ args = @('list','vdirs','/xml');     out = 'vdirs.xml' }
    @{ args = @('list','modules','/xml');   out = 'modules.xml' }
    @{ args = @('list','wp');               out = 'wp.txt' }
    @{ args = @('list','config','-section:system.webServer/modules');                    out = 'config-modules.txt' }
    @{ args = @('list','config','-section:system.webServer/globalModules');              out = 'config-globalModules.txt' }
    @{ args = @('list','config','-section:system.webServer/datadogRum');                 out = 'config-datadogRum.txt' }
    @{ args = @('list','config','-section:system.applicationHost/datadogRumServer');     out = 'config-datadogRumServer.txt' }
  )
  foreach ($c in $cmds) {
    Invoke-Appcmd -ArgList $c.args -OutFile (Join-Path $base $c.out) | Out-Null
  }
}

# ============================================================
# 6. per-site web.configs (parsed from applicationHost.config XML)
# ============================================================

if (-not $NoWebConfigs) {
  Invoke-Step 'per-site web.configs' {
    $appHost = "$env:SystemRoot\System32\inetsrv\config\applicationHost.config"
    if (-not (Test-Path -LiteralPath $appHost)) { return }

    [xml]$xml = Get-Content -LiteralPath $appHost -Raw -ErrorAction Stop
    $sitesNode = $xml.configuration.'system.applicationHost'.sites
    if (-not $sitesNode) { return }

    $copied = @()
    foreach ($s in @($sitesNode.site)) {
      foreach ($app in @($s.application)) {
        foreach ($vdir in @($app.virtualDirectory)) {
          $physical = $vdir.physicalPath
          if (-not $physical) { continue }
          $resolved = [System.Environment]::ExpandEnvironmentVariables($physical)
          $configFile = Join-Path $resolved 'web.config'

          $row = [ordered]@{
            site         = $s.name
            app          = $app.path
            vdir         = $vdir.path
            physicalPath = $resolved
            webconfig    = $configFile
            present      = (Test-Path -LiteralPath $configFile)
            size_bytes   = $null
            copied       = $false
            reason       = $null
          }

          if (-not $row.present) {
            $row.reason = 'no web.config in physical path'
            $copied += [pscustomobject]$row
            continue
          }

          $size = (Get-Item -LiteralPath $configFile).Length
          $row.size_bytes = $size
          if ($size -gt 1MB) {
            $row.reason = "skipped: $size bytes > 1MB cap"
            $copied += [pscustomobject]$row
            continue
          }

          $safe = ConvertTo-SafeFileName "$($s.name)__$($app.path)__$($vdir.path)"
          $dest = Join-Path $OutputDir "webconfigs/$safe.web.config"
          try {
            Copy-Item -LiteralPath $configFile -Destination $dest -Force
            $row.copied = $true
          } catch {
            $row.reason = "error: $($_.Exception.Message)"
          }
          $copied += [pscustomobject]$row
        }
      }
    }
    Save-Json -Path (Join-Path $OutputDir 'webconfigs/_index.json') -Object $copied
  }
}

# ============================================================
# 7. Datadog RUM binaries + ps modules
# ============================================================

Invoke-Step 'datadog rum binaries & modules' {
  $candidates = @(
    "$env:ProgramFiles\Datadog\Datadog RUM\injector_IIS.dll"
    "$env:ProgramFiles\Datadog\Datadog RUM\injectbrowsersdk.dll"
    "$env:ProgramFiles\Datadog\Datadog RUM\RUMTools.ps1"
    "$env:SystemRoot\System32\inetsrv\injector_IIS.dll"
    "$env:SystemRoot\System32\inetsrv\injectbrowsersdk.dll"
    "$env:ProgramFiles\WindowsPowerShell\Modules\Datadog.RUM\Datadog.RUM.psm1"
    "$env:ProgramFiles\WindowsPowerShell\Modules\Datadog.RUM\Datadog.RUM.psd1"
  )
  $rows = foreach ($c in $candidates) {
    if (-not (Test-Path -LiteralPath $c)) {
      [pscustomobject]@{ path = $c; present = $false }
      continue
    }
    $f = Get-Item -LiteralPath $c
    $vi = $f.VersionInfo
    $hash = $null
    try { $hash = (Get-FileHash -LiteralPath $c -Algorithm SHA256).Hash } catch {}
    $aclLines = @()
    try { $aclLines = (Get-Acl -LiteralPath $c).AccessToString -split "`n" } catch {}
    [pscustomobject]@{
      path             = $c
      present          = $true
      size_bytes       = $f.Length
      last_write_utc   = $f.LastWriteTimeUtc.ToString('o')
      file_version     = $vi.FileVersion
      product_version  = $vi.ProductVersion
      file_description = $vi.FileDescription
      sha256           = $hash
      acl              = $aclLines
    }
  }
  Save-Json -Path (Join-Path $OutputDir 'binaries/_index.json') -Object $rows
}

# ============================================================
# 8. registry
# ============================================================

Invoke-Step 'registry' {
  $rkeys = @(
    'HKLM:\Software\Datadog\Datadog RUM'
    'HKLM:\Software\Datadog\Datadog Agent'
    'HKLM:\SOFTWARE\Microsoft\InetStp'
    'HKLM:\SOFTWARE\Wow6432Node\Microsoft\InetStp'
  )
  foreach ($k in $rkeys) {
    $name = ConvertTo-SafeFileName $k
    $out = Join-Path $OutputDir "registry/$name.txt"
    try {
      if (Test-Path -LiteralPath $k) {
        Get-ItemProperty -LiteralPath $k -ErrorAction Stop |
          Format-List | Out-String |
          Set-Content -LiteralPath $out -Encoding UTF8
      } else {
        Set-Content -LiteralPath $out -Value "key not present: $k" -Encoding UTF8
      }
    } catch {
      Set-Content -LiteralPath $out -Value "error: $($_.Exception.Message)" -Encoding UTF8
    }
  }

  $uninstall = Get-ItemProperty `
    HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*, `
    HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\* `
    -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -match 'Datadog' } |
    Select-Object DisplayName, DisplayVersion, Publisher, InstallDate, InstallLocation, UninstallString, PSPath
  Save-Json -Path (Join-Path $OutputDir 'registry/uninstall-datadog.json') -Object $uninstall
}

# ============================================================
# 9. event logs
# ============================================================

if (-not $NoEventLogs) {
  Invoke-Step "event log: Datadog-RUM-Instrumentation (max $MaxEvents)" {
    $providerName = 'Datadog-RUM-Instrumentation'
    $providerExists = $false
    try {
      $null = Get-WinEvent -ListProvider $providerName -ErrorAction Stop
      $providerExists = $true
    } catch {
      Save-Text -Path (Join-Path $OutputDir 'events/datadog-rum.MISSING.txt') `
                -Text "Provider $providerName not registered on this host."
    }

    if ($providerExists) {
      try {
        $events = Get-WinEvent -ProviderName $providerName -MaxEvents $MaxEvents -ErrorAction Stop
        $events |
          Select-Object TimeCreated, Id, LevelDisplayName, ProviderName, Message |
          Export-Csv -LiteralPath (Join-Path $OutputDir 'events/datadog-rum.csv') -NoTypeInformation -Encoding UTF8

        $summary = $events | Group-Object Id | ForEach-Object {
          [pscustomobject]@{
            Id                 = $_.Name
            Count              = $_.Count
            FirstSeenUtc       = ($_.Group | Measure-Object -Property TimeCreated -Minimum).Minimum.ToUniversalTime().ToString('o')
            LastSeenUtc        = ($_.Group | Measure-Object -Property TimeCreated -Maximum).Maximum.ToUniversalTime().ToString('o')
            SampleMessage      = ($_.Group | Select-Object -First 1).Message
          }
        }
        Save-Json -Path (Join-Path $OutputDir 'events/datadog-rum.summary.json') -Object $summary
      } catch {
        if ($_.Exception.Message -match 'No events were found') {
          Save-Text -Path (Join-Path $OutputDir 'events/datadog-rum.EMPTY.txt') `
                    -Text "Provider exists but no events present."
        } else {
          throw
        }
      }
    }
  }

  Invoke-Step 'event log: WAS / IIS / system errors (last 24h)' {
    $since = (Get-Date).AddHours(-24)
    $logs = @('System','Application','Microsoft-Windows-IIS-Logging/Logs','Microsoft-IIS-Configuration/Operational')
    foreach ($log in $logs) {
      $safe = ConvertTo-SafeFileName $log
      $out = Join-Path $OutputDir "events/$safe.csv"
      try {
        $events = Get-WinEvent -FilterHashtable @{ LogName = $log; StartTime = $since } -MaxEvents 500 -ErrorAction Stop
        $events |
          Where-Object {
            $_.LevelDisplayName -in 'Error','Warning' -or
            $_.ProviderName -match 'IIS|WAS|w3svc|Datadog'
          } |
          Select-Object TimeCreated, Id, LevelDisplayName, ProviderName, Message |
          Export-Csv -LiteralPath $out -NoTypeInformation -Encoding UTF8
      } catch {
        Save-Text -Path "$out.MISSING.txt" -Text "log unavailable: $($_.Exception.Message)"
      }
    }
  }
}

# ============================================================
# 10. isolation: container & AppContainer detection
# ============================================================
# Generic, fault-tolerant. Each signal is independently optional —
# any failure to query is recorded but never aborts the step.

Invoke-Step 'isolation: container & app-container' {
  $report = [ordered]@{
    is_in_server_container    = $null   # tri-state: $true / $false / $null (unknown)
    server_container_signal   = $null
    host_runs_containers      = $null
    host_container_signals    = @()
    is_in_hyper_v_vm          = $null
    hyper_v_signal            = $null
    is_in_app_container       = $null
    w3wp_app_container_status = @()
    notes                     = @()
  }

  # --- definitive signal: cexecsvc is the in-container init service ---
  try {
    $cexec = Get-Service -Name cexecsvc -ErrorAction Stop
    $report.is_in_server_container  = $true
    $report.server_container_signal = "cexecsvc service present (status=$($cexec.Status))"
  } catch [Microsoft.PowerShell.Commands.ServiceCommandException] {
    $report.is_in_server_container  = $false
    $report.server_container_signal = 'cexecsvc service absent'
  } catch {
    $report.notes += "cexecsvc check error: $($_.Exception.Message)"
  }

  # --- host-side signals (does this host run/manage containers?) ---
  foreach ($s in 'vmcompute','hns','hcs','vmms') {
    try {
      $svc = Get-Service -Name $s -ErrorAction Stop
      if ($svc.Status -eq 'Running') {
        $report.host_container_signals += "service:$s running"
      } else {
        $report.host_container_signals += "service:$s present ($($svc.Status))"
      }
    } catch [Microsoft.PowerShell.Commands.ServiceCommandException] {
      # absent — not a signal, skip
    } catch {
      $report.notes += "service $s check error: $($_.Exception.Message)"
    }
  }
  if (Test-Path -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\HostComputeService') {
    $report.host_container_signals += 'registry:HKLM\SYSTEM\CurrentControlSet\Control\HostComputeService present'
  }
  if (Test-Path -LiteralPath 'C:\ProgramData\Microsoft\Windows\Containers') {
    $report.host_container_signals += 'path:C:\ProgramData\Microsoft\Windows\Containers exists'
  }
  try {
    $vEth = @(Get-NetAdapter -ErrorAction Stop | Where-Object { $_.Name -match 'vEthernet \(' -and $_.Name -match 'Container' })
    if ($vEth.Count -gt 0) {
      $report.host_container_signals += ("vEthernet adapters: " + ($vEth.Name -join ', '))
    }
  } catch {
    # Get-NetAdapter not present on Server Core minimal — non-fatal
  }
  $report.host_runs_containers = ($report.host_container_signals.Count -gt 0)

  # --- Hyper-V hint (not a container per se, but useful context) ---
  try {
    $cs = Get-CimInstance Win32_ComputerSystem -ErrorAction Stop
    $report.is_in_hyper_v_vm = [bool]$cs.HypervisorPresent
    $report.hyper_v_signal   = "manufacturer=$($cs.Manufacturer); model=$($cs.Model); hypervisorPresent=$($cs.HypervisorPresent)"
  } catch {
    $report.notes += "Win32_ComputerSystem query error: $($_.Exception.Message)"
  }

  # --- AppContainer detection via TokenIsAppContainer (info class 29) ---
  # Wrap the P/Invoke separately so a load failure on a stripped Windows SKU
  # doesn't cascade into the rest of the step.
  $tokenApiOk = $false
  try {
    Add-Type -ErrorAction Stop -Namespace RumFlareIso -Name Tok -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError=true)]
[return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
public static extern bool OpenProcessToken(System.IntPtr ProcessHandle, uint DesiredAccess, out System.IntPtr TokenHandle);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError=true)]
[return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
public static extern bool GetTokenInformation(System.IntPtr h, int cls, out int info, int len, out int ret);
[System.Runtime.InteropServices.DllImport("kernel32.dll")]
public static extern System.IntPtr GetCurrentProcess();
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError=true)]
public static extern System.IntPtr OpenProcess(uint access, [System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)] bool inherit, int pid);
[System.Runtime.InteropServices.DllImport("kernel32.dll")]
public static extern bool CloseHandle(System.IntPtr h);
'@
    $tokenApiOk = $true
  } catch {
    $report.notes += "AppContainer P/Invoke load failed: $($_.Exception.Message)"
  }

  if ($tokenApiOk) {
    function script:Test-AppContainer {
      param([System.IntPtr]$ProcessHandle)
      $tok = [System.IntPtr]::Zero
      if (-not [RumFlareIso.Tok]::OpenProcessToken($ProcessHandle, 0x0008, [ref]$tok)) { return $null }
      try {
        $val = 0; $ret = 0
        if ([RumFlareIso.Tok]::GetTokenInformation($tok, 29, [ref]$val, 4, [ref]$ret)) {
          return [bool]$val
        }
        return $null
      } finally {
        [RumFlareIso.Tok]::CloseHandle($tok) | Out-Null
      }
    }

    try {
      $report.is_in_app_container = Test-AppContainer ([RumFlareIso.Tok]::GetCurrentProcess())
    } catch {
      $report.notes += "self-token query error: $($_.Exception.Message)"
    }

    foreach ($w in @(Get-Process -Name w3wp -ErrorAction SilentlyContinue)) {
      $row = [ordered]@{ pid = $w.Id; is_app_container = $null; error = $null }
      try {
        $h = [RumFlareIso.Tok]::OpenProcess(0x0400, $false, $w.Id)  # PROCESS_QUERY_INFORMATION
        if ($h -eq [System.IntPtr]::Zero) {
          $row.error = 'OpenProcess failed (need admin?)'
        } else {
          try { $row.is_app_container = Test-AppContainer $h }
          finally { [RumFlareIso.Tok]::CloseHandle($h) | Out-Null }
        }
      } catch { $row.error = $_.Exception.Message }
      $report.w3wp_app_container_status += [pscustomobject]$row
    }
  }

  Save-Json -Path (Join-Path $OutputDir 'isolation/_summary.json') -Object $report
}

# ============================================================
# 11. network sanity
# ============================================================

Invoke-Step 'network: listening ports + agent reach' {
  try {
    Get-NetTCPConnection -State Listen -ErrorAction Stop |
      Where-Object { $_.LocalPort -in 80,443,8080,8081,8125,8126,8200 } |
      Select-Object LocalAddress, LocalPort, OwningProcess |
      Format-Table -AutoSize | Out-String |
      Set-Content -LiteralPath (Join-Path $OutputDir 'network/listening-relevant.txt') -Encoding UTF8
  } catch {
    Save-Text -Path (Join-Path $OutputDir 'network/listening-relevant.txt') `
              -Text "Get-NetTCPConnection failed: $($_.Exception.Message)"
  }

  $report = [ordered]@{}
  $probes = @(
    @{ Host = '127.0.0.1'; Port = 8126; Label = 'datadog-agent-apm' }
    @{ Host = '127.0.0.1'; Port = 80;   Label = 'iis-http' }
    @{ Host = '127.0.0.1'; Port = 443;  Label = 'iis-https' }
  )
  foreach ($p in $probes) {
    $r = [ordered]@{ host = $p.Host; port = $p.Port; tcpTestSucceeded = $false; error = $null }
    try {
      $tcp = New-Object System.Net.Sockets.TcpClient
      $iar = $tcp.BeginConnect($p.Host, $p.Port, $null, $null)
      if ($iar.AsyncWaitHandle.WaitOne(1500, $false)) {
        try { $tcp.EndConnect($iar); $r.tcpTestSucceeded = $true } catch { $r.error = $_.Exception.Message }
      } else {
        $r.error = 'timeout (1.5s)'
      }
      $tcp.Close()
    } catch {
      $r.error = $_.Exception.Message
    }
    $report[$p.Label] = $r
  }
  Save-Json -Path (Join-Path $OutputDir 'network/probes.json') -Object $report
}

# ============================================================
# 12. close out
# ============================================================

$script:Manifest.finishedUtc = (Get-Date).ToUniversalTime().ToString('o')
Save-Json -Path (Join-Path $OutputDir 'manifest.json') -Object $script:Manifest

if ($Zip) {
  $zipFull = "$OutputDir.zip"
  if (Test-Path -LiteralPath $zipFull) { Remove-Item -LiteralPath $zipFull -Force }
  try {
    Compress-Archive -Path (Join-Path $OutputDir '*') -DestinationPath $zipFull -CompressionLevel Optimal -ErrorAction Stop
    Write-Host ""
    Write-Host "Archive: $zipFull" -ForegroundColor Green
  } catch {
    Write-Fail "Compress-Archive failed: $($_.Exception.Message)"
  }
}

Write-Host ""
Write-Host "Done. Output: $($script:Manifest.outputDir)" -ForegroundColor Green
$failed = @($script:Manifest.steps | Where-Object { -not $_.ok })
if ($failed.Count -gt 0) {
  Write-Host "Failed steps: $($failed.Count)" -ForegroundColor Yellow
  $failed | ForEach-Object { Write-Host ("  - {0}: {1}" -f $_.label, $_.error) }
} else {
  Write-Host "All steps OK." -ForegroundColor Green
}
