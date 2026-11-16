# Run as Administrator on the isolated Windows Server 2022 IIS test host.
# Synthetic YAML and package links only; never edits the host's stable config.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TestRoot,
    [Parameter(Mandatory = $true)][string]$BundlePath,
    [Parameter(Mandatory = $true)][string]$RumDllPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$moduleName = 'DatadogIisRumAutoInject'
$originalModule = 'DDRumModule'
$poolName = 'DDRumAutoLifecyclePool'
$falsePoolName = 'DDRumAutoFalseLifecyclePool'
$fleetPoolName = 'DDRumAutoFleetLifecyclePool'
$x86PoolName = 'DDRumAutoX86LifecyclePool'
$primarySite = 'DDRumAutoLifecycle'
$coexistSite = 'DDRumAutoCoexist'
$falseSite = 'DDRumAutoFalseLifecycle'
$fleetSite = 'DDRumAutoFleetLifecycle'
$x86Site = 'DDRumAutoX86Lifecycle'
$root = Join-Path $TestRoot 'lifecycle'
$siteRoot = Join-Path $root 'sites'
$packageRoot = Join-Path $root 'packages\datadog-rum-inject'
$yamlPath = Join-Path $root 'synthetic-application_monitoring.yaml'
$falseYamlPath = Join-Path $root 'synthetic-disabled-application_monitoring.yaml'
$fleetYamlPath = Join-Path $root 'synthetic-fleet-application_monitoring.yaml'
$fleetPath = Join-Path $root 'absent-fleet-application_monitoring.yaml'
$hook = Join-Path $BundlePath 'Invoke-IisRumLifecycle.ps1'
$manifest = Join-Path $BundlePath 'manifest.json'
$bootstrap = Join-Path $BundlePath 'ddiisrum-bootstrap.dll'
$rumDll = $RumDllPath
$schema = Join-Path $BundlePath 'RUM_schema.xml'
$appHost = Join-Path $env:windir 'System32\inetsrv\config\applicationHost.config'
$installedSchema = Join-Path $env:windir 'System32\inetsrv\config\schema\RUM_schema.xml'
$durableRoot = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'Datadog\Installer\iis-rum-bootstrap'
$bootstrapHash = (Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash.ToLowerInvariant()
$durableVersion = Join-Path $durableRoot $bootstrapHash
$defaultPackage = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'Datadog\Installer\packages\datadog-rum-inject'
$results = New-Object 'System.Collections.Generic.List[object]'
$failure = $null
$backupName = 'DDRumLifecycle-' + (Get-Date -Format 'yyyyMMddHHmmss')
$createdRoot = $false
$createdPools = New-Object 'System.Collections.Generic.List[string]'
$createdPrimary = $false
$createdCoexist = $false
$createdFalse = $false
$createdFleet = $false
$createdX86 = $false
$createdBackup = $false
$installerAttempted = $false
$originalImage = $null
$originalPrecondition = $null
$originalHash = $null
$schemaHash = $null
$appHostHash = $null
$controlHash = $null
$durableRootExisted = $false
$durableVersionExisted = $false

function Add-Result([string]$Name, [bool]$Passed, [string]$Detail) {
    $script:results.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
    if (-not $Passed) { throw "$Name failed: $Detail" }
}

function Invoke-Lifecycle([string]$Action, [bool]$ExpectSuccess) {
    $saved = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass `
            -File $script:hook -Action $Action -ContractVersion 1 2>&1 | Out-String
        $exitCode = $LASTEXITCODE
    }
    finally { $ErrorActionPreference = $saved }
    if ($ExpectSuccess -and $exitCode -ne 0) { throw "Lifecycle hook exit $exitCode`: $output" }
    if (-not $ExpectSuccess -and $exitCode -eq 0) { throw "Lifecycle hook unexpectedly succeeded: $output" }
    return [pscustomobject]@{ code = $exitCode; output = $output.Trim() }
}

function Get-ModuleEntries([string]$Name) {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $config = $manager.GetApplicationHostConfiguration()
        $global = @($config.GetSection('system.webServer/globalModules').GetCollection() |
            Where-Object { $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name })
        $rootEntries = @($config.GetSection('system.webServer/modules').GetCollection() |
            Where-Object { $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name })
        return [pscustomobject]@{
            globalCount = $global.Count; rootCount = $rootEntries.Count;
            image = if ($global.Count -eq 1) { [string]$global[0].GetAttributeValue('image') } else { $null };
            globalPrecondition = if ($global.Count -eq 1) { [string]$global[0].GetAttributeValue('preCondition') } else { $null };
            rootPrecondition = if ($rootEntries.Count -eq 1) { [string]$rootEntries[0].GetAttributeValue('preCondition') } else { $null }
        }
    }
    finally { $manager.Dispose() }
}

function Get-Marker([string]$Name) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey('SOFTWARE\Datadog\AutoInject\IIS RUM', $false)
        if ($null -eq $key) { return $null }
        try { return $key.GetValue($Name, $null) }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Remove-OwnedModule {
    $entries = Get-ModuleEntries $moduleName
    if ($entries.rootCount -eq 1) {
        Invoke-Lifecycle 'uninstall' $true | Out-Null
        $entries = Get-ModuleEntries $moduleName
    }
    if ($entries.globalCount -ne 1 -or $entries.rootCount -ne 0 -or
        (Get-Marker "$moduleName.ModulePath") -cne $entries.image) {
        throw 'Test bootstrap global entry is not exactly installer-owned; preserving it.'
    }
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $collection = $manager.GetApplicationHostConfiguration().GetSection('system.webServer/globalModules').GetCollection()
        $entry = @($collection | Where-Object {
            $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $moduleName
        })
        if ($entry.Count -ne 1 -or [string]$entry[0].GetAttributeValue('image') -cne $entries.image) {
            throw 'Test global entry changed; preserving it.'
        }
        $collection.Remove($entry[0])
        $manager.CommitChanges()
    }
    finally { $manager.Dispose() }
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey('SOFTWARE\Datadog\AutoInject\IIS RUM', $true)
        if ($null -ne $key) {
            try { $key.DeleteValue("$moduleName.ModulePath", $false) }
            finally { $key.Dispose() }
        }
    }
    finally { $base.Dispose() }
}

function Add-TestPool([string]$Name, [string]$LocalYaml, [bool]$Is32Bit,
                      [string]$FleetYaml = $fleetPath) {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $pool = $manager.ApplicationPools.Add($Name)
        $pool.ManagedRuntimeVersion = ''
        $pool.Enable32BitAppOnWin64 = $Is32Bit
        $entry = @($manager.GetApplicationHostConfiguration().GetSection(
            'system.applicationHost/applicationPools').GetCollection() | Where-Object {
                $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name
            })
        if ($entry.Count -ne 1) { throw 'New lifecycle pool missing from IIS configuration.' }
        $vars = $entry[0].GetCollection('environmentVariables')
        foreach ($pair in @(
            @('DD_IIS_RUM_PACKAGE_ROOT', $packageRoot),
            @('DD_IIS_RUM_STABLE_CONFIG_LOCAL', $LocalYaml),
            @('DD_IIS_RUM_STABLE_CONFIG_FLEET', $FleetYaml))) {
            $variable = $vars.CreateElement('add')
            $variable.SetAttributeValue('name', $pair[0])
            $variable.SetAttributeValue('value', $pair[1])
            $vars.Add($variable) | Out-Null
        }
        $manager.CommitChanges()
        $script:createdPools.Add($Name)
    }
    finally { $manager.Dispose() }
}

function Write-Page([string]$Directory, [string]$RumElement, [string]$ExtraModules) {
    [IO.Directory]::CreateDirectory($Directory) | Out-Null
    [IO.File]::WriteAllText((Join-Path $Directory 'index.html'),
        '<!doctype html><html><head><title>RUM lifecycle</title></head><body>fixture</body></html>',
        [Text.Encoding]::UTF8)
    [IO.File]::WriteAllText((Join-Path $Directory 'web.config'),
        "<configuration><system.webServer>$ExtraModules$RumElement</system.webServer></configuration>",
        [Text.Encoding]::UTF8)
}

function Get-Page([int]$Port, [string]$Path) {
    return Invoke-WebRequest -UseBasicParsing -TimeoutSec 15 -Uri "http://127.0.0.1:$Port/$Path"
}

function Assert-Page([string]$Name, [int]$Port, [string]$Path, [bool]$Injected,
                     [hashtable]$Expected, [string[]]$Absent) {
    $response = Get-Page $Port $Path
    $snippetMatches = [regex]::Matches([string]$response.Content,
        'window\.DD_RUM\.init\((\{.*?\})\);', [Text.RegularExpressions.RegexOptions]::Singleline)
    $header = [string]$response.Headers['x-datadog-rum-injected']
    $valid = $response.StatusCode -eq 200
    if ($Injected) {
        $valid = $valid -and $header -eq '1' -and $snippetMatches.Count -eq 1
        if ($valid) {
            $options = ConvertFrom-Json -InputObject $snippetMatches[0].Groups[1].Value
            foreach ($key in $Expected.Keys) {
                $property = $options.PSObject.Properties[$key]
                if ($null -eq $property -or [string]$property.Value -cne [string]$Expected[$key]) { $valid = $false }
            }
            foreach ($key in $Absent) {
                if ($null -ne $options.PSObject.Properties[$key]) { $valid = $false }
            }
        }
    }
    else { $valid = $valid -and [string]::IsNullOrEmpty($header) -and $snippetMatches.Count -eq 0 }
    Add-Result $Name $valid "status=$($response.StatusCode), header=$header, snippets=$($snippetMatches.Count)"
}

function Get-WorkerPid {
    $workers = @(Get-CimInstance Win32_Process -Filter "Name='w3wp.exe'" | Where-Object {
        [string]$_.CommandLine -like "*$poolName*"
    })
    if ($workers.Count -ne 1) { throw "Expected one worker for $poolName, found $($workers.Count)." }
    return [int]$workers[0].ProcessId
}

function Get-LoadedPackageModules([int]$ProcessId) {
    return @(Get-Process -Id $ProcessId -Module | Where-Object {
        [string]$_.FileName -like '*\lifecycle\packages\datadog-rum-inject\*\injector_IIS.dll'
    } | ForEach-Object FileName)
}

function Assert-SameWorker([string]$Name, [int]$ExpectedPid) {
    $actual = Get-WorkerPid
    Add-Result $Name ($actual -eq $ExpectedPid) "worker PID=$actual"
}

# Keep requests flowing across a package link change. An in-flight response
# may finish under its previous policy; every response must stay well formed.
function Assert-TrafficDuringChange([string]$Name, [scriptblock]$Change) {
    $key = [guid]::NewGuid().ToString('N')
    $ready = Join-Path $root "traffic-$key.ready"
    $stop = Join-Path $root "traffic-$key.stop"
    $job = Start-Job -ArgumentList 18086, $ready, $stop -ScriptBlock {
        param($Port, $Ready, $Stop)
        [IO.File]::WriteAllText($Ready, 'ready')
        $count = 0
        $failures = New-Object 'System.Collections.Generic.List[string]'
        while (-not (Test-Path -LiteralPath $Stop) -and $count -lt 100) {
            try {
                $response = Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 -Uri "http://127.0.0.1:$Port/index.html"
                $snippets = [regex]::Matches([string]$response.Content,
                    'window\.DD_RUM\.init\((\{.*?\})\);',
                    [Text.RegularExpressions.RegexOptions]::Singleline).Count
                $header = [string]$response.Headers['x-datadog-rum-injected']
                if ($response.StatusCode -ne 200 -or $snippets -gt 1 -or
                    ($header -eq '1' -and $snippets -ne 1) -or
                    ($header -ne '1' -and $snippets -ne 0)) {
                    $failures.Add("status=$($response.StatusCode), header=$header, snippets=$snippets")
                }
            }
            catch { $failures.Add($_.Exception.Message) }
            $count++
            Start-Sleep -Milliseconds 50
        }
        [pscustomobject]@{ count = $count; failures = $failures.ToArray() }
    }
    try {
        $deadline = (Get-Date).AddSeconds(10)
        while (-not (Test-Path -LiteralPath $ready) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 50
        }
        if (-not (Test-Path -LiteralPath $ready)) { throw 'Traffic job did not start.' }
        Start-Sleep -Milliseconds 250
        & $Change
        Start-Sleep -Milliseconds 250
    }
    finally {
        [IO.File]::WriteAllText($stop, 'stop')
        Wait-Job -Job $job -Timeout 15 | Out-Null
        if ($job.State -eq 'Running') { Stop-Job -Job $job }
        $traffic = Receive-Job -Job $job
        Remove-Job -Job $job -Force
        Remove-Item -LiteralPath $ready, $stop -Force -ErrorAction SilentlyContinue
    }
    Add-Result $Name ($traffic.count -ge 3 -and $traffic.failures.Count -eq 0) `
        "requests=$($traffic.count), failures=$($traffic.failures -join '; ')"
}

function New-PackageVersion([string]$Version) {
    $directory = Join-Path $packageRoot "versions\$Version"
    [IO.Directory]::CreateDirectory((Join-Path $directory 'iis')) | Out-Null
    [IO.File]::WriteAllText((Join-Path $directory 'version'), "$Version`n", [Text.Encoding]::ASCII)
    Copy-Item -LiteralPath $rumDll -Destination (Join-Path $directory 'iis\injector_IIS.dll')
    return $directory
}

function Publish-Package([string]$Slot, [string]$Target) {
    $link = Join-Path $packageRoot $Slot
    if (Test-Path -LiteralPath $link) { throw "Active package link already exists: $link" }
    New-Item -ItemType Junction -Path $link -Target $Target | Out-Null
    if (((Get-Item -LiteralPath $link).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
        throw "Package active path is not a junction: $link"
    }
}

function Withdraw-Package([string]$Slot) {
    $link = Join-Path $packageRoot $Slot
    if (-not (Test-Path -LiteralPath $link)) { throw "Package active link missing: $link" }
    if (((Get-Item -LiteralPath $link).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
        throw "Refusing to remove non-junction path: $link"
    }
    & $env:ComSpec /d /c ('rmdir "' + $link + '"') | Out-Null
    if ($LASTEXITCODE -ne 0 -or (Test-Path -LiteralPath $link)) {
        throw "Could not remove test-owned package junction: $link"
    }
}

function Recycle-PrivatePool {
    $oldPids = @(Get-CimInstance Win32_Process -Filter "Name='w3wp.exe'" | Where-Object {
        [string]$_.CommandLine -like "*$poolName*"
    } | ForEach-Object { [int]$_.ProcessId })
    Restart-WebAppPool -Name $poolName
    $deadline = (Get-Date).AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $remaining = @($oldPids | Where-Object {
            $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue)
        })
    } until ($remaining.Count -eq 0 -or (Get-Date) -gt $deadline)
    if ($remaining.Count -ne 0) { throw 'Former private pool workers did not stop for recycle.' }
}

function Test-DurableAcl([string]$Path) {
    $acl = Get-Acl -LiteralPath $Path
    $owner = ([Security.Principal.NTAccount]$acl.Owner).Translate(
        [Security.Principal.SecurityIdentifier]).Value
    $entries = @($acl.Access | ForEach-Object {
        [pscustomobject]@{
            sid = $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;
            rights = $_.FileSystemRights; type = $_.AccessControlType
        }
    })
    $admins = 'S-1-5-32-544'
    $system = 'S-1-5-18'
    $auth = 'S-1-5-11'
    $writeRights = [int][Security.AccessControl.FileSystemRights]::WriteData -bor
        [int][Security.AccessControl.FileSystemRights]::AppendData -bor
        [int][Security.AccessControl.FileSystemRights]::WriteAttributes -bor
        [int][Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
        [int][Security.AccessControl.FileSystemRights]::Delete -bor
        [int][Security.AccessControl.FileSystemRights]::ChangePermissions -bor
        [int][Security.AccessControl.FileSystemRights]::TakeOwnership
    $unexpected = @($entries | Where-Object {
        $_.type -eq [Security.AccessControl.AccessControlType]::Allow -and
        (([int]$_.rights -band $writeRights) -ne 0) -and $_.sid -notin @($admins, $system)
    })
    $hasAdmins = @($entries | Where-Object { $_.sid -eq $admins -and
        $_.type -eq [Security.AccessControl.AccessControlType]::Allow }).Count -gt 0
    $hasSystem = @($entries | Where-Object { $_.sid -eq $system -and
        $_.type -eq [Security.AccessControl.AccessControlType]::Allow }).Count -gt 0
    $hasAuthRead = @($entries | Where-Object { $_.sid -eq $auth -and
        $_.type -eq [Security.AccessControl.AccessControlType]::Allow -and
        (([int]$_.rights -band [int][Security.AccessControl.FileSystemRights]::ReadAndExecute) -ne 0) }).Count -gt 0
    return $owner -eq $admins -and $hasAdmins -and $hasSystem -and $hasAuthRead -and $unexpected.Count -eq 0
}

try {
    foreach ($path in @($hook, $manifest, $bootstrap, $rumDll, $schema, $installedSchema)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required test file missing: $path" }
    }
    Add-Type -Path (Join-Path $env:windir 'System32\inetsrv\Microsoft.Web.Administration.dll')
    Import-Module WebAdministration
    if (Test-Path $root) { throw "Lifecycle test root already exists: $root" }
    foreach ($name in @($primarySite, $coexistSite, $falseSite, $fleetSite, $x86Site)) {
        if (Get-Website -Name $name -ErrorAction SilentlyContinue) { throw "Test site exists: $name" }
    }
    foreach ($name in @($poolName, $falsePoolName, $fleetPoolName, $x86PoolName)) {
        if (Test-Path "IIS:\AppPools\$name") { throw "Test pool exists: $name" }
    }
    $before = Get-ModuleEntries $moduleName
    if ($before.globalCount -ne 0 -or $before.rootCount -ne 0) { throw 'Bootstrap module already registered.' }
    if ((Get-Marker "$moduleName.ModulePath") -or (Get-Marker "$moduleName.RootEntry")) {
        throw 'Bootstrap ownership marker already exists.'
    }
    if ((Test-Path (Join-Path $defaultPackage 'stable')) -or
        (Test-Path (Join-Path $defaultPackage 'experiment'))) {
        throw 'A default-location RUM package is active; test root registration would affect existing pools.'
    }
    $manual = Get-ModuleEntries $originalModule
    if ($manual.globalCount -ne 1 -or $manual.rootCount -ne 1) { throw 'Original RUM module missing.' }
    $originalImage = $manual.image
    $originalPrecondition = $manual.rootPrecondition
    $originalHash = (Get-FileHash -LiteralPath $originalImage -Algorithm SHA256).Hash
    $schemaHash = (Get-FileHash -LiteralPath $installedSchema -Algorithm SHA256).Hash
    $appHostHash = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
    $control = Get-Page 8099 ''
    if ($control.StatusCode -ne 200 -or [string]$control.Headers['x-datadog-rum-injected'] -ne '1') {
        throw 'Original :8099 RUM positive control missing.'
    }
    $controlHash = [BitConverter]::ToString(([Security.Cryptography.SHA256]::Create()).ComputeHash(
        [Text.Encoding]::UTF8.GetBytes([string]$control.Content)))
    $durableRootExisted = Test-Path -LiteralPath $durableRoot
    $durableVersionExisted = Test-Path -LiteralPath $durableVersion
    if ($durableVersionExisted) {
        $existingDurableDll = Join-Path $durableVersion 'ddiisrum-bootstrap.dll'
        if (-not (Test-Path -LiteralPath $existingDurableDll -PathType Leaf) -or
            (Get-FileHash -LiteralPath $existingDurableDll -Algorithm SHA256).Hash.ToLowerInvariant() -ne $bootstrapHash -or
            -not (Test-DurableAcl $existingDurableDll)) {
            throw "Existing content-addressed bootstrap path is not the expected protected DLL: $durableVersion"
        }
    }

    & (Join-Path $env:windir 'System32\inetsrv\appcmd.exe') add backup $backupName | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'IIS backup failed.' }
    $createdBackup = $true
    [IO.Directory]::CreateDirectory($root) | Out-Null
    $createdRoot = $true
    [IO.Directory]::CreateDirectory($packageRoot) | Out-Null
    $yaml = @'
apm_configuration_default:
  DD_RUM_ENABLED: true
  DD_RUM_APPLICATION_ID: TEST_STABLE_APP
  DD_RUM_CLIENT_TOKEN: TEST_STABLE_TOKEN
  DD_RUM_SITE: datadoghq.com
  DD_RUM_SERVICE: stable-service
'@
    [IO.File]::WriteAllText($yamlPath, $yaml, [Text.Encoding]::UTF8)
    $falseYaml = @'
apm_configuration_default:
  DD_RUM_ENABLED: false
  DD_RUM_APPLICATION_ID: TEST_FALSE_APP
  DD_RUM_CLIENT_TOKEN: TEST_FALSE_TOKEN
  DD_RUM_SITE: datadoghq.com
  DD_RUM_SERVICE: false-service
'@
    [IO.File]::WriteAllText($falseYamlPath, $falseYaml, [Text.Encoding]::UTF8)
    $fleetYaml = @'
apm_configuration_default:
  DD_RUM_ENABLED: true
  DD_RUM_APPLICATION_ID: TEST_FLEET_APP
  DD_RUM_CLIENT_TOKEN: TEST_FLEET_TOKEN
  DD_RUM_SITE: datadoghq.com
  DD_RUM_SERVICE: fleet-service
'@
    [IO.File]::WriteAllText($fleetYamlPath, $fleetYaml, [Text.Encoding]::UTF8)
    Add-TestPool $poolName $yamlPath $false
    Add-TestPool $falsePoolName $falseYamlPath $false
    Add-TestPool $fleetPoolName $yamlPath $false $fleetYamlPath
    Add-TestPool $x86PoolName $yamlPath $true
    $removeManual = "<modules><remove name=`"$originalModule`" /></modules>"
    $caching = '<caching enabled="true" enableKernelCache="true"><profiles><add extension=".html" policy="CacheUntilChange" kernelCachePolicy="CacheUntilChange" /></profiles></caching>'
    Write-Page (Join-Path $siteRoot 'primary') '' ($removeManual + $caching)
    Write-Page (Join-Path $siteRoot 'primary\partial') '<datadogRum><option name="service" value="manual-service" /></datadogRum>' ''
    Write-Page (Join-Path $siteRoot 'primary\disabled') '<datadogRum enabled="false" />' ''
    Write-Page (Join-Path $siteRoot 'primary\clear') '<datadogRum><clear /><option name="applicationId" value="CLEAR_APP" /><option name="clientToken" value="CLEAR_TOKEN" /><option name="site" value="datadoghq.com" /></datadogRum>' ''
    Write-Page (Join-Path $siteRoot 'primary\remove') '<datadogRum><option name="applicationId" value="REMOVE_APP" /><option name="clientToken" value="REMOVE_TOKEN" /><option name="site" value="datadoghq.com" /><option name="service" value="parent-service" /></datadogRum>' ''
    Write-Page (Join-Path $siteRoot 'primary\remove\child') '<datadogRum><remove name="service" /></datadogRum>' ''
    Write-Page (Join-Path $siteRoot 'primary\module_excluded') '' "<modules><remove name=`"$moduleName`" /></modules>"
    Write-Page (Join-Path $siteRoot 'false') '<datadogRum enabled="true" />' $removeManual
    Write-Page (Join-Path $siteRoot 'fleet') '' $removeManual
    Write-Page (Join-Path $siteRoot 'x86') '' $removeManual
    $manualRum = '<datadogRum enabled="true" version="5"><option name="applicationId" value="MANUAL_COEXIST_APP" /><option name="clientToken" value="MANUAL_COEXIST_TOKEN" /><option name="site" value="datadoghq.com" /><option name="service" value="manual-coexist" /></datadogRum>'
    Write-Page (Join-Path $siteRoot 'coexist') $manualRum ''
    New-Website -Name $primarySite -Port 18086 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'primary') -ApplicationPool $poolName | Out-Null
    $createdPrimary = $true
    New-Website -Name $coexistSite -Port 18087 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'coexist') -ApplicationPool $poolName | Out-Null
    $createdCoexist = $true
    New-Website -Name $falseSite -Port 18088 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'false') -ApplicationPool $falsePoolName | Out-Null
    $createdFalse = $true
    New-Website -Name $fleetSite -Port 18090 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'fleet') -ApplicationPool $fleetPoolName | Out-Null
    $createdFleet = $true
    New-Website -Name $x86Site -Port 18089 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'x86') -ApplicationPool $x86PoolName | Out-Null
    $createdX86 = $true
    foreach ($child in @('partial', 'disabled', 'clear', 'remove', 'module_excluded')) {
        New-WebApplication -Site $primarySite -Name $child -PhysicalPath (Join-Path (Join-Path $siteRoot 'primary') $child) -ApplicationPool $poolName | Out-Null
    }
    foreach ($repeat in 1..3) {
        Assert-Page "cache-warm before bootstrap registration $repeat" 18086 'index.html' $false @{} @()
    }
    $preInstallPids = @(Get-CimInstance Win32_Process -Filter "Name='w3wp.exe'" | Where-Object {
        [string]$_.CommandLine -like "*$poolName*"
    } | ForEach-Object { [int]$_.ProcessId })

    $installerAttempted = $true
    Invoke-Lifecycle 'install' $true | Out-Null
    Invoke-Lifecycle 'install' $true | Out-Null
    $installed = Get-ModuleEntries $moduleName
    Add-Result 'normal repeated install registers once' ($installed.globalCount -eq 1 -and
        $installed.rootCount -eq 1 -and $installed.globalPrecondition -eq 'bitness64') "global=$($installed.globalCount), root=$($installed.rootCount)"
    Add-Result 'durable bootstrap hash and ACL' ((Get-FileHash -LiteralPath $installed.image -Algorithm SHA256).Hash.ToLowerInvariant() -eq $bootstrapHash -and
        $installed.image.StartsWith($durableVersion, [StringComparison]::OrdinalIgnoreCase) -and
        (Test-DurableAcl $installed.image)) 'content-addressed protected DLL; owner/admin/system/RX ACL verified'

    Assert-Page 'package absent leaves primary unmodified' 18086 '' $false @{} @()
    $retirementDeadline = (Get-Date).AddSeconds(30)
    do {
        $preInstallAlive = @($preInstallPids | Where-Object {
            $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue)
        })
        if ($preInstallAlive.Count -gt 0) { Start-Sleep -Milliseconds 250 }
    } until ($preInstallAlive.Count -eq 0 -or (Get-Date) -gt $retirementDeadline)
    Add-Result 'registration retired old private worker' ($preInstallAlive.Count -eq 0) "old PIDs=$($preInstallPids -join ',')"

    foreach ($repeat in 1..3) {
        Assert-Page "cache-warm absent response $repeat" 18086 'index.html' $false @{} @()
    }
    Assert-Page 'manual RUM survives package absence' 18087 '' $true @{ applicationId = 'MANUAL_COEXIST_APP'; clientToken = 'MANUAL_COEXIST_TOKEN'; service = 'manual-coexist' } @()
    Assert-Page 'explicit enable still needs package' 18088 '' $false @{} @()
    Assert-Page 'fleet configuration still needs package' 18090 '' $false @{} @()
    Assert-Page 'x86 worker absent serves HTML' 18089 '' $false @{} @()
    $pid1 = Get-WorkerPid
    Add-Result 'absent package DLL was not loaded' (@(Get-LoadedPackageModules $pid1).Count -eq 0) "worker PID=$pid1"

    $stableTarget = New-PackageVersion '1.0.0'
    Assert-TrafficDuringChange 'ongoing traffic safe during package publish' {
        Publish-Package 'stable' $stableTarget
    }
    Assert-Page 'publish activates existing app' 18086 '' $true @{ applicationId = 'TEST_STABLE_APP'; clientToken = 'TEST_STABLE_TOKEN'; service = 'stable-service' } @()
    Assert-Page 'cached same URL activates after publish' 18086 'index.html' $true @{ applicationId = 'TEST_STABLE_APP' } @()
    Assert-SameWorker 'publish does not require worker restart' $pid1
    Add-Result 'stable runtime loaded from immutable target' (@(Get-LoadedPackageModules $pid1 | Where-Object {
        $_ -like '*versions\1.0.0\iis\injector_IIS.dll'
    }).Count -eq 1) 'loaded path points to version 1.0.0'
    Assert-Page 'stable config merges manual option' 18086 'partial/' $true @{ applicationId = 'TEST_STABLE_APP'; clientToken = 'TEST_STABLE_TOKEN'; service = 'manual-service' } @()
    Assert-Page 'manual enabled false survives stable config' 18086 'disabled/' $false @{} @()
    Assert-Page 'manual clear suppresses stable options' 18086 'clear/' $true @{ applicationId = 'CLEAR_APP'; clientToken = 'CLEAR_TOKEN' } @('service')
    Assert-Page 'manual remove preserves inherited manual credentials' 18086 'remove/child/' $true @{ applicationId = 'REMOVE_APP'; clientToken = 'REMOVE_TOKEN' } @('service')
    Assert-Page 'module exclusion suppresses auto injection' 18086 'module_excluded/' $false @{} @()
    Assert-Page 'explicit true overrides disabled stable config' 18088 '' $true @{ applicationId = 'TEST_FALSE_APP'; clientToken = 'TEST_FALSE_TOKEN'; service = 'false-service' } @()
    Assert-Page 'fleet stable YAML overrides local YAML' 18090 '' $true @{ applicationId = 'TEST_FLEET_APP'; clientToken = 'TEST_FLEET_TOKEN'; service = 'fleet-service' } @()
    Assert-Page 'x86 worker skips x64 bootstrap' 18089 '' $false @{} @()
    Assert-Page 'manual and bootstrap coexist once' 18087 '' $true @{ applicationId = 'MANUAL_COEXIST_APP'; clientToken = 'MANUAL_COEXIST_TOKEN'; service = 'manual-coexist' } @()
    Write-Page (Join-Path $siteRoot 'primary\future') '' ''
    New-WebApplication -Site $primarySite -Name 'future' -PhysicalPath (Join-Path $siteRoot 'primary\future') -ApplicationPool $poolName | Out-Null
    Assert-Page 'future app inherits automatic injection' 18086 'future/' $true @{ applicationId = 'TEST_STABLE_APP'; clientToken = 'TEST_STABLE_TOKEN' } @()

    Assert-TrafficDuringChange 'ongoing traffic safe during package withdrawal' {
        Withdraw-Package 'stable'
    }
    Assert-Page 'withdraw stops same-worker injection' 18086 '' $false @{} @()
    Assert-Page 'cached same URL stops after withdrawal' 18086 'index.html' $false @{} @()
    Assert-SameWorker 'withdraw does not require worker restart' $pid1
    Assert-Page 'manual RUM survives package withdrawal' 18087 '' $true @{ applicationId = 'MANUAL_COEXIST_APP'; clientToken = 'MANUAL_COEXIST_TOKEN' } @()
    Assert-TrafficDuringChange 'ongoing traffic safe during same-target restoration' {
        Publish-Package 'stable' $stableTarget
    }
    Assert-Page 'same target restoration resumes injection' 18086 '' $true @{ applicationId = 'TEST_STABLE_APP' } @()
    Assert-Page 'cached same URL resumes after restoration' 18086 'index.html' $true @{ applicationId = 'TEST_STABLE_APP' } @()
    Assert-SameWorker 'same target reuse stays in worker' $pid1

    Withdraw-Package 'stable'
    Recycle-PrivatePool
    Assert-Page 'new worker absent stays unmodified' 18086 '' $false @{} @()
    $pid2 = Get-WorkerPid
    Add-Result 'new worker was created' ($pid2 -ne $pid1) "worker PID=$pid2"
    Add-Result 'application-stop cleanup after withdrawal completed' ($pid2 -ne $pid1) `
        'old worker retired and replacement served an unmodified response'
    Add-Result 'new worker did not load payload' (@(Get-LoadedPackageModules $pid2).Count -eq 0) 'no package DLL in worker'
    Publish-Package 'stable' $stableTarget
    Assert-Page 'new worker activates on package restore' 18086 '' $true @{ applicationId = 'TEST_STABLE_APP' } @()
    Assert-SameWorker 'restoration stayed in new worker' $pid2

    $experimentTarget = New-PackageVersion '2.0.0'
    Publish-Package 'experiment' $experimentTarget
    Assert-Page 'version switch disables old runtime' 18086 '' $false @{} @()
    Assert-SameWorker 'version switch does not silently recycle' $pid2
    Recycle-PrivatePool
    Assert-Page 'new worker selects complete experiment' 18086 '' $true @{ applicationId = 'TEST_STABLE_APP' } @()
    $pid3 = Get-WorkerPid
    Add-Result 'experiment runtime loaded from version 2.0.0' (@(Get-LoadedPackageModules $pid3 | Where-Object {
        $_ -like '*versions\2.0.0\iis\injector_IIS.dll'
    }).Count -eq 1) 'loaded path points to version 2.0.0'
    Withdraw-Package 'experiment'
    Assert-Page 'fall back to different target needs recycle' 18086 '' $false @{} @()
    Recycle-PrivatePool
    Assert-Page 'recycled worker selects stable again' 18086 '' $true @{ applicationId = 'TEST_STABLE_APP' } @()

    Invoke-Lifecycle 'uninstall' $true | Out-Null
    $uninstalled = Get-ModuleEntries $moduleName
    Add-Result 'normal uninstall removes inherited activation' ($uninstalled.globalCount -eq 1 -and
        $uninstalled.rootCount -eq 0) "global=$($uninstalled.globalCount), root=$($uninstalled.rootCount)"
    Assert-Page 'normal uninstall stops automatic injection' 18086 '' $false @{} @()
    Assert-Page 'normal uninstall leaves manual RUM' 18087 '' $true @{ applicationId = 'MANUAL_COEXIST_APP'; clientToken = 'MANUAL_COEXIST_TOKEN' } @()
}
catch { $failure = $_.Exception.Message }
finally {
    $ownedWorkerPids = @(Get-CimInstance Win32_Process -Filter "Name='w3wp.exe'" |
        Where-Object {
            $commandLine = [string]$_.CommandLine
            @($createdPools | Where-Object { $commandLine -like "*$_*" }).Count -gt 0
        } | ForEach-Object { [int]$_.ProcessId })
    if ($createdX86) { try { Remove-Website -Name $x86Site -ErrorAction Stop } catch { $failure = "$failure; cleanup x86 site: $($_.Exception.Message)" } }
    if ($createdFleet) { try { Remove-Website -Name $fleetSite -ErrorAction Stop } catch { $failure = "$failure; cleanup fleet site: $($_.Exception.Message)" } }
    if ($createdFalse) { try { Remove-Website -Name $falseSite -ErrorAction Stop } catch { $failure = "$failure; cleanup false site: $($_.Exception.Message)" } }
    if ($createdCoexist) { try { Remove-Website -Name $coexistSite -ErrorAction Stop } catch { $failure = "$failure; cleanup coexist site: $($_.Exception.Message)" } }
    if ($createdPrimary) { try { Remove-Website -Name $primarySite -ErrorAction Stop } catch { $failure = "$failure; cleanup primary site: $($_.Exception.Message)" } }
    foreach ($name in $createdPools) {
        try { Stop-WebAppPool -Name $name -ErrorAction SilentlyContinue; Remove-WebAppPool -Name $name -ErrorAction Stop }
        catch { $failure = "$failure; cleanup pool $name`: $($_.Exception.Message)" }
    }
    $workerDeadline = (Get-Date).AddSeconds(20)
    do {
        $aliveWorkers = @($ownedWorkerPids | Where-Object {
            $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue)
        })
        if ($aliveWorkers.Count -gt 0) { Start-Sleep -Milliseconds 250 }
    } until ($aliveWorkers.Count -eq 0 -or (Get-Date) -gt $workerDeadline)
    if ($aliveWorkers.Count -gt 0) {
        $failure = "$failure; test workers did not exit: $($aliveWorkers -join ',')"
    }
    if ($installerAttempted) {
        try {
            $entries = Get-ModuleEntries $moduleName
            if ($entries.globalCount -gt 0 -or $entries.rootCount -gt 0) { Remove-OwnedModule }
        }
        catch { $failure = "$failure; cleanup module: $($_.Exception.Message)" }
    }
    if ($createdRoot -and (Test-Path $root) -and $aliveWorkers.Count -eq 0) {
        try {
            foreach ($slot in @('experiment', 'stable')) {
                $link = Join-Path $packageRoot $slot
                if (Test-Path -LiteralPath $link) { Withdraw-Package $slot }
            }
            Remove-Item -LiteralPath $root -Recurse -Force
        }
        catch { $failure = "$failure; cleanup files: $($_.Exception.Message)" }
    }
    if ($null -ne $originalImage) {
        try {
            $after = Get-ModuleEntries $originalModule
            $manualUnchanged = $after.globalCount -eq 1 -and $after.rootCount -eq 1 -and
                $after.image -eq $originalImage -and $after.rootPrecondition -eq $originalPrecondition -and
                (Get-FileHash -LiteralPath $after.image -Algorithm SHA256).Hash -eq $originalHash -and
                (Get-FileHash -LiteralPath $installedSchema -Algorithm SHA256).Hash -eq $schemaHash
            Add-Result 'original RUM module and schema preserved' $manualUnchanged 'original registration and hashes unchanged'
            $positive = Get-Page 8099 ''
            $afterHash = [BitConverter]::ToString(([Security.Cryptography.SHA256]::Create()).ComputeHash(
                [Text.Encoding]::UTF8.GetBytes([string]$positive.Content)))
            Add-Result 'original RUM positive control preserved' ($positive.StatusCode -eq 200 -and
                [string]$positive.Headers['x-datadog-rum-injected'] -eq '1' -and
                $afterHash -eq $controlHash) 'status/header/body hash unchanged'
            Add-Result 'IIS configuration bytes restored' ((Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash -eq $appHostHash) 'applicationHost.config SHA256 matches preflight'
        }
        catch { $failure = "$failure; original state check: $($_.Exception.Message)" }
    }
    $summary = [pscustomobject]@{ host = $env:COMPUTERNAME; backup = $backupName;
        passed = [string]::IsNullOrEmpty($failure); failure = $failure;
        resultsCount = $results.Count; results = $results.ToArray();
        artifacts = [pscustomobject]@{
            bootstrapSha256 = $bootstrapHash;
            rumDllSha256 = (Get-FileHash -LiteralPath $rumDll -Algorithm SHA256).Hash.ToLowerInvariant();
            hookSha256 = (Get-FileHash -LiteralPath $hook -Algorithm SHA256).Hash.ToLowerInvariant();
            manifestSha256 = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant();
            appHostBeforeSha256 = $appHostHash;
            appHostAfterSha256 = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash.ToLowerInvariant();
            installedSchemaSha256 = $schemaHash;
            durableBootstrapPath = $durableVersion
        } }
    if ($summary.passed -and $createdBackup) {
        & (Join-Path $env:windir 'System32\inetsrv\appcmd.exe') delete backup $backupName | Out-Null
    }
    $summary | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath (Join-Path $TestRoot 'lifecycle-results.json') -Encoding UTF8
    $summary | ConvertTo-Json -Depth 7
}
if ($failure) { exit 1 }
