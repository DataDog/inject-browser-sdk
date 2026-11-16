# Run on an isolated Windows Server 2022 IIS host as Administrator.
# Uses synthetic RUM IDs and restores every IIS object it creates.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TestRoot,
    [Parameter(Mandatory = $true)][string]$InstallerScript,
    [string]$ModuleName = 'DDRumAutoTestModule'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$installer = $InstallerScript
$module = Join-Path $TestRoot 'injector_IIS.dll'
$schema = Join-Path $TestRoot 'RUM_schema.xml'
$siteRoot = Join-Path $TestRoot 'sites'
$appHost = Join-Path $env:windir 'System32\inetsrv\config\applicationHost.config'
$installedSchema = Join-Path $env:windir 'System32\inetsrv\config\schema\RUM_schema.xml'
$originalModule = 'DDRumModule'
$siteNames = @('DDRumAutoIntegration', 'DDRumAutoFalseDefault', 'DDRumAutoX86', 'DDRumAutoRegistry')
$poolNames = @('DDRumAutoTestPool', 'DDRumAutoFalsePool', 'DDRumAutoX86Pool', 'DDRumAutoRegistryPool')
$createdSites = New-Object 'System.Collections.Generic.List[string]'
$createdPools = New-Object 'System.Collections.Generic.List[string]'
$results = New-Object 'System.Collections.Generic.List[object]'
$failure = $null
$backupName = 'DDRumAutoTest-' + (Get-Date -Format 'yyyyMMddHHmmss')
$originalDllHash = $null
$originalSchemaHash = $null
$originalImage = $null
$originalPrecondition = $null
$createdSiteRoot = $false
$createdBackup = $false
$installerAttempted = $false
$registryInstallAttempted = $false
$originalDefault = $null
$originalDefaultKind = $null
$originalDefaultExists = $false
$capturedDefault = $false
$positiveControlHash = $null

function Get-DefaultValue {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey('SOFTWARE\Datadog\IIS RUM', $false)
        if ($null -eq $key) { return [pscustomobject]@{ exists = $false; value = $null; kind = $null } }
        try {
            if ($key.GetValueNames() -notcontains 'DefaultConfig') {
                return [pscustomobject]@{ exists = $false; value = $null; kind = $null }
            }
            return [pscustomobject]@{ exists = $true;
                value = $key.GetValue('DefaultConfig', $null, 'DoNotExpandEnvironmentNames');
                kind = $key.GetValueKind('DefaultConfig') }
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Restore-DefaultValue {
    $current = Get-DefaultValue
    if ($script:originalDefaultExists -eq $current.exists -and
        (-not $script:originalDefaultExists -or
            ($current.kind -eq $script:originalDefaultKind -and $current.value -ceq $script:originalDefault))) { return }
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.CreateSubKey('SOFTWARE\Datadog\IIS RUM')
        try {
            if ($script:originalDefaultExists) {
                $key.SetValue('DefaultConfig', $script:originalDefault, $script:originalDefaultKind)
            }
            else { $key.DeleteValue('DefaultConfig', $false) }
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Set-TestDefaultValue([string]$Value) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.CreateSubKey('SOFTWARE\Datadog\IIS RUM')
        try { $key.SetValue('DefaultConfig', $Value, [Microsoft.Win32.RegistryValueKind]::String) }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Set-TestDefaultDword([int]$Value) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.CreateSubKey('SOFTWARE\Datadog\IIS RUM')
        try { $key.SetValue('DefaultConfig', $Value, [Microsoft.Win32.RegistryValueKind]::DWord) }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Add-Result([string]$Name, [bool]$Passed, [string]$Detail) {
    $script:results.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
    if (-not $Passed) { throw "$Name failed: $Detail" }
}

function Invoke-Installer([string[]]$Arguments, [bool]$ExpectSuccess) {
    if ($Arguments.Count -lt 2 -or $Arguments[0] -ne 'iis-rum') {
        throw 'Expected an IIS RUM action.'
    }
    $parameters = @{ Action = $Arguments[1]; ModuleName = $ModuleName }
    for ($i = 2; $i -lt $Arguments.Count; $i += 2) {
        if ($i + 1 -ge $Arguments.Count) { throw 'Incomplete installer argument.' }
        switch ($Arguments[$i]) {
            '--module' { $parameters.ModulePath = $Arguments[$i + 1] }
            '--schema' { $parameters.SchemaPath = $Arguments[$i + 1] }
            '--config' { $parameters.ConfigPath = $Arguments[$i + 1] }
            '--name' { $parameters.ModuleName = $Arguments[$i + 1] }
            default { throw "Unexpected installer argument: $($Arguments[$i])" }
        }
    }
    $code = 0
    $output = ''
    try { $output = (& $script:installer @parameters | Out-String).Trim() }
    catch { $code = 1; $output = $_.Exception.Message }
    if ($ExpectSuccess -and $code -ne 0) { throw "Installer failed: $output" }
    if (-not $ExpectSuccess -and $code -eq 0) { throw "Installer unexpectedly succeeded: $output" }
    return [pscustomobject]@{ code = $code; output = $output }
}

function Get-ModuleEntries([string]$Name) {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $config = $manager.GetApplicationHostConfiguration()
        $global = @($config.GetSection('system.webServer/globalModules').GetCollection() |
            Where-Object { $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name })
        $root = @($config.GetSection('system.webServer/modules').GetCollection() |
            Where-Object { $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name })
        return [pscustomobject]@{ globalCount = $global.Count; rootCount = $root.Count;
            image = if ($global.Count -eq 1) { [string]$global[0].GetAttributeValue('image') } else { $null };
            preCondition = if ($root.Count -eq 1) { [string]$root[0].GetAttributeValue('preCondition') } else { $null } }
    }
    finally { $manager.Dispose() }
}

function Set-TestRootPrecondition([string]$Value) {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $collection = $manager.GetApplicationHostConfiguration().GetSection('system.webServer/modules').GetCollection()
        $entry = @($collection | Where-Object {
            $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $script:ModuleName
        })
        if ($entry.Count -ne 1) { throw 'Test root module entry is missing or duplicated.' }
        $entry[0].SetAttributeValue('preCondition', $Value)
        $manager.CommitChanges()
    }
    finally { $manager.Dispose() }
}

function Add-TestPool([string]$Name, [string]$Config, [bool]$Is32Bit) {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $pool = $manager.ApplicationPools.Add($Name)
        $pool.ManagedRuntimeVersion = ''
        $pool.Enable32BitAppOnWin64 = $Is32Bit
        $pools = $manager.GetApplicationHostConfiguration().GetSection('system.applicationHost/applicationPools').GetCollection()
        $entry = @($pools | Where-Object {
            $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $Name
        })
        if ($entry.Count -ne 1) { throw "Cannot find new pool $Name in IIS configuration." }
        if (-not [string]::IsNullOrEmpty($Config)) {
            $vars = $entry[0].GetCollection('environmentVariables')
            $variable = $vars.CreateElement('add')
            $variable.SetAttributeValue('name', 'DD_IIS_RUM_CONFIG')
            $variable.SetAttributeValue('value', $Config)
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
        '<!doctype html><html><head><title>DD RUM integration</title></head><body>fixture</body></html>',
        [Text.Encoding]::UTF8)
    $config = "<configuration><system.webServer>$ExtraModules$RumElement</system.webServer></configuration>"
    [IO.File]::WriteAllText((Join-Path $Directory 'web.config'), $config, [Text.Encoding]::UTF8)
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
    if ($Injected) {
        $valid = $response.StatusCode -eq 200 -and $header -eq '1' -and $snippetMatches.Count -eq 1
        if ($valid) {
            $options = ConvertFrom-Json -InputObject $snippetMatches[0].Groups[1].Value
            foreach ($key in $Expected.Keys) {
                $property = $options.PSObject.Properties[$key]
                if ($null -eq $property -or [string]$property.Value -ne [string]$Expected[$key]) { $valid = $false }
            }
            foreach ($key in $Absent) {
                if ($null -ne $options.PSObject.Properties[$key]) { $valid = $false }
            }
        }
        Add-Result $Name $valid "status=$($response.StatusCode), header=$header, snippets=$($snippetMatches.Count)"
    }
    else {
        $valid = $response.StatusCode -eq 200 -and [string]::IsNullOrEmpty($header) -and $snippetMatches.Count -eq 0
        Add-Result $Name $valid "status=$($response.StatusCode), header=$header, snippets=$($snippetMatches.Count)"
    }
}

function Remove-TestGlobalModule {
    $manager = New-Object Microsoft.Web.Administration.ServerManager
    try {
        $config = $manager.GetApplicationHostConfiguration()
        $collection = $config.GetSection('system.webServer/globalModules').GetCollection()
        $entry = @($collection | Where-Object {
            $_.ElementTagName -eq 'add' -and [string]$_.GetAttributeValue('name') -eq $script:ModuleName
        })
        if ($entry.Count -eq 1 -and
            [string]::Equals([string]$entry[0].GetAttributeValue('image'), $script:module,
                [StringComparison]::OrdinalIgnoreCase)) {
            $collection.Remove($entry[0])
            $manager.CommitChanges()
        }
    }
    finally { $manager.Dispose() }
}

try {
    foreach ($path in @($installer, $module, $schema, $installedSchema)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required test file missing: $path" }
    }
    Add-Type -Path (Join-Path $env:windir 'System32\inetsrv\Microsoft.Web.Administration.dll')
    Import-Module WebAdministration
    foreach ($name in $siteNames) { if (Get-Website -Name $name -ErrorAction SilentlyContinue) { throw "Site already exists: $name" } }
    foreach ($name in $poolNames) { if (Test-Path "IIS:\AppPools\$name") { throw "Pool already exists: $name" } }
    if (Test-Path $siteRoot) { throw "Test site directory already exists: $siteRoot" }
    $priorTestModule = Get-ModuleEntries $ModuleName
    if ($priorTestModule.globalCount -ne 0 -or $priorTestModule.rootCount -ne 0) {
        throw "Test module $ModuleName already exists; refusing to alter it."
    }
    $markerBase = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $markerKey = $markerBase.OpenSubKey('SOFTWARE\Datadog\AutoInject\IIS RUM', $false)
        if ($null -ne $markerKey) {
            try {
                foreach ($marker in @("$ModuleName.ModulePath", "$ModuleName.RootEntry", "$ModuleName.DefaultConfigSha256")) {
                    if ($null -ne $markerKey.GetValue($marker, $null)) { throw "Test ownership marker $marker already exists." }
                }
            }
            finally { $markerKey.Dispose() }
        }
    }
    finally { $markerBase.Dispose() }
    $before = Get-ModuleEntries $originalModule
    if ($before.globalCount -ne 1 -or $before.rootCount -ne 1) { throw 'Expected existing DDRumModule not present exactly once.' }
    $originalDllHash = (Get-FileHash -LiteralPath $before.image -Algorithm SHA256).Hash
    $originalSchemaHash = (Get-FileHash -LiteralPath $installedSchema -Algorithm SHA256).Hash
    $originalImage = $before.image
    $originalPrecondition = $before.preCondition
    $default = Get-DefaultValue
    $originalDefaultExists = $default.exists
    $originalDefault = $default.value
    $originalDefaultKind = $default.kind
    $capturedDefault = $true
    $positiveControl = Get-Page 8099 ''
    if ($positiveControl.StatusCode -ne 200 -or [string]$positiveControl.Headers['x-datadog-rum-injected'] -ne '1') {
        throw 'Existing LocationTest RUM positive control is not active.'
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $positiveControlHash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes([string]$positiveControl.Content))) }
    finally { $sha.Dispose() }
    & (Join-Path $env:windir 'System32\inetsrv\appcmd.exe') add backup $backupName | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'IIS configuration backup failed.' }
    $createdBackup = $true

    $baseline = [ordered]@{ enabled = $true; version = 5; options = [ordered]@{
        applicationId = 'TEST_AUTO_APP'; clientToken = 'TEST_AUTO_TOKEN'; site = 'datadoghq.com';
        service = 'auto-service'; env = 'auto-env' } } | ConvertTo-Json -Compress -Depth 5
    $falseBaseline = [ordered]@{ enabled = $false; version = 5; options = [ordered]@{
        applicationId = 'TEST_FALSE_APP'; clientToken = 'TEST_FALSE_TOKEN'; site = 'datadoghq.com';
        service = 'false-service' } } | ConvertTo-Json -Compress -Depth 5
    Add-TestPool $poolNames[0] $baseline $false
    Add-TestPool $poolNames[1] $falseBaseline $false
    Add-TestPool $poolNames[2] $baseline $true
    Add-TestPool $poolNames[3] $null $false

    $removeOriginal = "<modules><remove name=`"$originalModule`" /></modules>"
    $createdSiteRoot = $true
    Write-Page (Join-Path $siteRoot 'primary') '' $removeOriginal
    Write-Page (Join-Path $siteRoot 'false') '<datadogRum enabled="true" />' $removeOriginal
    Write-Page (Join-Path $siteRoot 'x86') '' $removeOriginal
    Write-Page (Join-Path $siteRoot 'registry') '<datadogRum enabled="true" />' $removeOriginal
    $appConfigs = @{
        partial = '<datadogRum><option name="service" value="manual-service" /></datadogRum>';
        partial_appid = '<datadogRum><option name="applicationId" value="MANUAL_APP" /></datadogRum>';
        partial_token = '<datadogRum><option name="clientToken" value="MANUAL_TOKEN" /></datadogRum>';
        disabled = '<datadogRum enabled="false" />';
        clear = '<datadogRum><clear /><option name="applicationId" value="CLEAR_APP" /><option name="clientToken" value="CLEAR_TOKEN" /><option name="site" value="datadoghq.com" /></datadogRum>';
        module_excluded = '';
        inherit = '<datadogRum><option name="applicationId" value="PARENT_APP" /><option name="clientToken" value="PARENT_TOKEN" /><option name="service" value="parent-service" /><option name="site" value="datadoghq.com" /></datadogRum>'
    }
    foreach ($name in $appConfigs.Keys) {
        $mods = if ($name -eq 'module_excluded') { "<modules><remove name=`"$ModuleName`" /></modules>" } else { '' }
        Write-Page (Join-Path (Join-Path $siteRoot 'primary') $name) $appConfigs[$name] $mods
    }
    Write-Page (Join-Path $siteRoot 'primary\inherit\child') '<datadogRum><remove name="service" /><option name="service" value="child-service" /></datadogRum>' ''

    New-Website -Name $siteNames[0] -Port 18082 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'primary') -ApplicationPool $poolNames[0] | Out-Null
    $createdSites.Add($siteNames[0])
    New-Website -Name $siteNames[1] -Port 18083 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'false') -ApplicationPool $poolNames[1] | Out-Null
    $createdSites.Add($siteNames[1])
    New-Website -Name $siteNames[2] -Port 18084 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'x86') -ApplicationPool $poolNames[2] | Out-Null
    $createdSites.Add($siteNames[2])
    New-Website -Name $siteNames[3] -Port 18085 -IPAddress '127.0.0.1' -PhysicalPath (Join-Path $siteRoot 'registry') -ApplicationPool $poolNames[3] | Out-Null
    $createdSites.Add($siteNames[3])
    foreach ($name in $appConfigs.Keys) {
        New-WebApplication -Site $siteNames[0] -Name $name -PhysicalPath (Join-Path (Join-Path $siteRoot 'primary') $name) -ApplicationPool $poolNames[0] | Out-Null
    }

    $installArgs = @('iis-rum', 'install', '--module', $module, '--schema', $schema, '--name', $ModuleName)
    $installerAttempted = $true
    Invoke-Installer $installArgs $true | Out-Null
    Invoke-Installer $installArgs $true | Out-Null
    $entries = Get-ModuleEntries $ModuleName
    Add-Result 'repeat install has one registration' ($entries.globalCount -eq 1 -and $entries.rootCount -eq 1) "global=$($entries.globalCount), root=$($entries.rootCount)"

    $alternate = Join-Path $TestRoot 'alternate-injector_IIS.dll'
    Copy-Item -LiteralPath $module -Destination $alternate -Force
    $hashBeforeMismatch = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
    $bad = Invoke-Installer @('iis-rum', 'install', '--module', $alternate, '--schema', $schema, '--name', $ModuleName) $false
    $hashAfterMismatch = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
    Add-Result 'mismatched DLL rejected without mutation' ($hashBeforeMismatch -eq $hashAfterMismatch) "exit=$($bad.code)"

    Assert-Page 'root baseline' 18082 '' $true @{ applicationId = 'TEST_AUTO_APP'; clientToken = 'TEST_AUTO_TOKEN'; service = 'auto-service'; env = 'auto-env' } @()
    Assert-Page 'manual per-app option merges' 18082 'partial/' $true @{ applicationId = 'TEST_AUTO_APP'; clientToken = 'TEST_AUTO_TOKEN'; service = 'manual-service'; env = 'auto-env' } @()
    Assert-Page 'manual applicationId merges' 18082 'partial_appid/' $true @{ applicationId = 'MANUAL_APP'; clientToken = 'TEST_AUTO_TOKEN'; service = 'auto-service' } @()
    Assert-Page 'manual clientToken merges' 18082 'partial_token/' $true @{ applicationId = 'TEST_AUTO_APP'; clientToken = 'MANUAL_TOKEN'; service = 'auto-service' } @()
    Assert-Page 'manual enabled false' 18082 'disabled/' $false @{} @()
    Assert-Page 'clear suppresses baseline options' 18082 'clear/' $true @{ applicationId = 'CLEAR_APP'; clientToken = 'CLEAR_TOKEN' } @('service', 'env')
    Assert-Page 'module exclusion' 18082 'module_excluded/' $false @{} @()
    Assert-Page 'inherited parent options' 18082 'inherit/' $true @{ applicationId = 'PARENT_APP'; clientToken = 'PARENT_TOKEN'; service = 'parent-service' } @()
    Assert-Page 'child override after remove' 18082 'inherit/child/' $true @{ applicationId = 'PARENT_APP'; clientToken = 'PARENT_TOKEN'; service = 'child-service' } @('env')
    Assert-Page 'explicit true overrides false baseline' 18083 '' $true @{ applicationId = 'TEST_FALSE_APP'; clientToken = 'TEST_FALSE_TOKEN'; service = 'false-service' } @()
    Assert-Page 'x86 worker serves without x64 module' 18084 '' $false @{} @()

    Write-Page (Join-Path $siteRoot 'primary\future') '' ''
    New-WebApplication -Site $siteNames[0] -Name 'future' -PhysicalPath (Join-Path $siteRoot 'primary\future') -ApplicationPool $poolNames[0] | Out-Null
    Assert-Page 'new application inherits registration' 18082 'future/' $true @{ applicationId = 'TEST_AUTO_APP'; clientToken = 'TEST_AUTO_TOKEN' } @()

    Set-TestRootPrecondition 'bitness32'
    $bad = Invoke-Installer @('iis-rum', 'uninstall', '--name', $ModuleName) $false
    $entries = Get-ModuleEntries $ModuleName
    Add-Result 'manually changed root is preserved' ($entries.rootCount -eq 1 -and $entries.preCondition -eq 'bitness32') "exit=$($bad.code)"
    Set-TestRootPrecondition 'bitness64'
    Invoke-Installer @('iis-rum', 'uninstall', '--name', $ModuleName) $true | Out-Null
    $entries = Get-ModuleEntries $ModuleName
    Add-Result 'uninstall removes owned root only' ($entries.globalCount -eq 1 -and $entries.rootCount -eq 0) "global=$($entries.globalCount), root=$($entries.rootCount)"
    Assert-Page 'uninstall stops inherited injection' 18082 '' $false @{} @()

    $registryConfig = [ordered]@{ enabled = $false; version = 5; options = [ordered]@{
        applicationId = 'TEST_REGISTRY_APP'; clientToken = 'TEST_REGISTRY_TOKEN';
        site = 'datadoghq.com'; service = 'registry-service' } } | ConvertTo-Json -Compress -Depth 5
    $registryFile = Join-Path $TestRoot 'registry-baseline.json'
    [IO.File]::WriteAllText($registryFile, $registryConfig, [Text.Encoding]::UTF8)
    $registryArgs = @('iis-rum', 'install', '--module', $module, '--schema', $schema,
        '--config', $registryFile, '--name', $ModuleName)
    if ($originalDefaultExists) {
        $registryInstallAttempted = $true
        $appHostHash = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
        $bad = Invoke-Installer $registryArgs $false
        $preserved = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash -eq $appHostHash -and
            (Get-DefaultValue).value -ceq $originalDefault
        Add-Result 'unmanaged registry default rejects install' $preserved "exit=$($bad.code)"
    }
    else {
        $registryInstallAttempted = $true
        Invoke-Installer $registryArgs $true | Out-Null
        Assert-Page 'registry baseline in env-free pool' 18085 '' $true @{ applicationId = 'TEST_REGISTRY_APP'; clientToken = 'TEST_REGISTRY_TOKEN'; service = 'registry-service' } @()
        Assert-Page 'registry disabled baseline leaves existing site off' 80 '' $false @{} @()

        $caseConfig = $registryConfig.Replace('TEST_REGISTRY_TOKEN', 'test_registry_token')
        $caseFile = Join-Path $TestRoot 'registry-baseline-case.json'
        [IO.File]::WriteAllText($caseFile, $caseConfig, [Text.Encoding]::UTF8)
        $caseArgs = @('iis-rum', 'install', '--module', $module, '--schema', $schema,
            '--config', $caseFile, '--name', $ModuleName)
        Invoke-Installer $caseArgs $true | Out-Null
        $caseActual = Get-DefaultValue
        Add-Result 'owned registry case-only update' ($caseActual.exists -and
            $caseActual.kind -eq [Microsoft.Win32.RegistryValueKind]::String -and
            $caseActual.value -ceq $caseConfig) 'case-only JSON update preserved exactly'

        Invoke-Installer @('iis-rum', 'uninstall', '--name', $ModuleName) $true | Out-Null
        Add-Result 'registry default removed on uninstall' (-not (Get-DefaultValue).exists) 'owned registry value removed'

        Set-TestDefaultValue $registryConfig
        try {
            $appHostBeforeUnmanaged = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
            $bad = Invoke-Installer $caseArgs $false
            $unmanaged = Get-DefaultValue
            $appHostAfterUnmanaged = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
            Add-Result 'unmanaged registry case-only change rejected' ($bad.code -ne 0 -and
                $unmanaged.exists -and $unmanaged.value -ceq $registryConfig -and
                $unmanaged.kind -eq [Microsoft.Win32.RegistryValueKind]::String -and
                $appHostBeforeUnmanaged -eq $appHostAfterUnmanaged) "exit=$($bad.code), registry and IIS unchanged"

            Set-TestDefaultDword 1
            $appHostBeforeDword = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
            $bad = Invoke-Installer $caseArgs $false
            $dword = Get-DefaultValue
            $appHostAfterDword = (Get-FileHash -LiteralPath $appHost -Algorithm SHA256).Hash
            Add-Result 'non-string registry default preserved' ($bad.code -ne 0 -and
                $dword.exists -and $dword.value -eq 1 -and
                $dword.kind -eq [Microsoft.Win32.RegistryValueKind]::DWord -and
                $appHostBeforeDword -eq $appHostAfterDword) "exit=$($bad.code), registry and IIS unchanged"
        }
        finally { Restore-DefaultValue }
    }
}
catch {
    $failure = $_.Exception.Message
}
finally {
    foreach ($name in $createdSites) { try { Remove-Website -Name $name -ErrorAction Stop } catch { $failure = "$failure; cleanup site $name`: $($_.Exception.Message)" } }
    foreach ($name in $createdPools) { try { Remove-WebAppPool -Name $name -ErrorAction Stop } catch { $failure = "$failure; cleanup pool $name`: $($_.Exception.Message)" } }
    if ($installerAttempted) {
        try {
            $entries = Get-ModuleEntries $ModuleName
            if ($entries.rootCount -eq 1 -and $entries.preCondition -ne 'bitness64') { Set-TestRootPrecondition 'bitness64' }
            if ($entries.rootCount -eq 1) { Invoke-Installer @('iis-rum', 'uninstall', '--name', $ModuleName) $true | Out-Null }
            Remove-TestGlobalModule
        }
        catch { $failure = "$failure; cleanup module: $($_.Exception.Message)" }
        try {
            $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
            $owned = $base.OpenSubKey('SOFTWARE\Datadog\AutoInject\IIS RUM', $true)
            if ($null -ne $owned) { $owned.DeleteValue("$ModuleName.ModulePath", $false); $owned.Dispose() }
            $base.Dispose()
        }
        catch { $failure = "$failure; cleanup marker: $($_.Exception.Message)" }
    }
    if ($capturedDefault -and $registryInstallAttempted) {
        try {
            Restore-DefaultValue
            $currentDefault = Get-DefaultValue
            Add-Result 'registry value and kind restored' ($currentDefault.exists -eq $originalDefaultExists -and
                (-not $originalDefaultExists -or ($currentDefault.kind -eq $originalDefaultKind -and
                    $currentDefault.value -ceq $originalDefault))) 'previous registry state preserved'
        }
        catch { $failure = "$failure; cleanup registry: $($_.Exception.Message)" }
    }
    if ($createdSiteRoot -and (Test-Path $siteRoot)) {
        try { Remove-Item -LiteralPath $siteRoot -Recurse -Force }
        catch { $failure = "$failure; cleanup files: $($_.Exception.Message)" }
    }
    try {
        $after = Get-ModuleEntries $originalModule
        $sameOriginal = $after.globalCount -eq 1 -and $after.rootCount -eq 1 -and
            $after.image -eq $originalImage -and $after.preCondition -eq $originalPrecondition -and
            (Get-FileHash -LiteralPath $after.image -Algorithm SHA256).Hash -eq $originalDllHash -and
            (Get-FileHash -LiteralPath $installedSchema -Algorithm SHA256).Hash -eq $originalSchemaHash
        Add-Result 'original RUM module and schema preserved' $sameOriginal "global=$($after.globalCount), root=$($after.rootCount)"
        $positiveControlAfter = Get-Page 8099 ''
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $positiveAfterHash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes([string]$positiveControlAfter.Content))) }
        finally { $sha.Dispose() }
        Add-Result 'existing RUM positive control preserved' ($positiveControlAfter.StatusCode -eq 200 -and
            [string]$positiveControlAfter.Headers['x-datadog-rum-injected'] -eq '1' -and
            $positiveAfterHash -eq $positiveControlHash) 'status/header/body hash matched before and after'
    }
    catch { $failure = "$failure; original module check: $($_.Exception.Message)" }
    $summary = [pscustomobject]@{ host = $env:COMPUTERNAME; backup = $backupName; passed = [string]::IsNullOrEmpty($failure);
        failure = $failure; results = $results.ToArray() }
    if ($summary.passed -and $createdBackup) {
        & (Join-Path $env:windir 'System32\inetsrv\appcmd.exe') delete backup $backupName | Out-Null
    }
    $summary | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath (Join-Path $TestRoot 'integration-results.json') -Encoding UTF8
    $summary | ConvertTo-Json -Depth 7
}
if ($failure) { exit 1 }
