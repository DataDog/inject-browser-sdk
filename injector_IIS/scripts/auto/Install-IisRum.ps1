# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('install', 'uninstall')][string]$Action,
    [string]$ModulePath,
    [string]$SchemaPath,
    [string]$ConfigPath,
    [ValidatePattern('^[A-Za-z][A-Za-z0-9_.-]{0,127}$')][string]$ModuleName = 'DDRumModule',
    [switch]$Bootstrap
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RegistryKey([string]$SubKey, [bool]$Create) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Registry64)
    try {
        if ($Create) { return $base.CreateSubKey($SubKey) }
        return $base.OpenSubKey($SubKey, $false)
    }
    finally { $base.Dispose() }
}

function Get-Value([string]$SubKey, [string]$Name) {
    $key = Get-RegistryKey $SubKey $false
    if ($null -eq $key) { return $null }
    try { return $key.GetValue($Name, $null, 'DoNotExpandEnvironmentNames') }
    finally { $key.Dispose() }
}

function Get-ValueKind([string]$SubKey, [string]$Name) {
    $key = Get-RegistryKey $SubKey $false
    if ($null -eq $key) { return $null }
    try {
        if ($key.GetValueNames() -notcontains $Name) { return $null }
        return $key.GetValueKind($Name)
    }
    finally { $key.Dispose() }
}

function Set-Value([string]$SubKey, [string]$Name, [string]$Value) {
    $key = Get-RegistryKey $SubKey $true
    try { $key.SetValue($Name, $Value, [Microsoft.Win32.RegistryValueKind]::String) }
    finally { $key.Dispose() }
}

function Remove-Value([string]$SubKey, [string]$Name) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($SubKey, $true)
        if ($null -eq $key) { return }
        try { $key.DeleteValue($Name, $false) }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Get-Hash([string]$Value) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value))).Replace('-', '') }
    finally { $sha.Dispose() }
}

function Get-Entry($Collection, [string]$Name) {
    foreach ($entry in $Collection) {
        if ($entry.ElementTagName -eq 'add' -and [string]$entry.GetAttributeValue('name') -eq $Name) {
            return $entry
        }
    }
    return $null
}

function Same-Path([string]$First, [string]$Second) {
    $firstPath = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($First))
    $secondPath = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($Second))
    return [string]::Equals($firstPath, $secondPath, [StringComparison]::OrdinalIgnoreCase)
}

function Assert-CompatibleModule([string]$Path, [string]$CapabilityExport) {
    $stream = [IO.File]::OpenRead($Path)
    $reader = New-Object IO.BinaryReader($stream)
    try {
        if ($stream.Length -lt 0x40 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw 'IIS RUM module is not a PE DLL.'
        }
        [void]$stream.Seek(0x3C, [IO.SeekOrigin]::Begin)
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 0 -or $peOffset -gt $stream.Length - 6) {
            throw 'IIS RUM module has an invalid PE header.'
        }
        [void]$stream.Seek($peOffset, [IO.SeekOrigin]::Begin)
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw 'IIS RUM module must be a 64-bit Windows DLL.'
        }
    }
    finally { $reader.Dispose() }

    if (-not ([Management.Automation.PSTypeName]'IisRumNativeExports').Type) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class IisRumNativeExports {
    [DllImport("kernel32.dll", EntryPoint="LoadLibraryExW", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr LoadLibraryEx(string path, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Ansi, SetLastError=true)]
    public static extern IntPtr GetProcAddress(IntPtr module, string name);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool FreeLibrary(IntPtr module);
}
'@
    }
    # DONT_RESOLVE_DLL_REFERENCES maps exports without running DllMain or loading imports.
    $handle = [IisRumNativeExports]::LoadLibraryEx($Path, [IntPtr]::Zero, 1)
    if ($handle -eq [IntPtr]::Zero) {
        throw "Could not inspect IIS RUM DLL exports: Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())."
    }
    try {
        if ([IisRumNativeExports]::GetProcAddress($handle, 'RegisterModule') -eq [IntPtr]::Zero) {
            throw 'IIS RUM DLL does not export RegisterModule.'
        }
        if ([IisRumNativeExports]::GetProcAddress($handle, $CapabilityExport) -eq [IntPtr]::Zero) {
            throw "IIS DLL lacks $CapabilityExport; use the matching patched module."
        }
    }
    finally { [void][IisRumNativeExports]::FreeLibrary($handle) }
}

function Protect-BootstrapPath([string]$Path, [bool]$Directory) {
    $admins = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $iisUsers = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-568')
    $authenticatedUsers = [Security.Principal.SecurityIdentifier]::new('S-1-5-11')
    if ($Directory) {
        $acl = New-Object Security.AccessControl.DirectorySecurity
        $inherit = ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
            [Security.AccessControl.InheritanceFlags]::ObjectInherit)
    }
    else {
        $acl = New-Object Security.AccessControl.FileSecurity
        $inherit = [Security.AccessControl.InheritanceFlags]::None
    }
    $acl.SetOwner($admins)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($principal in @($admins, $system)) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            $principal, [Security.AccessControl.FileSystemRights]::FullControl,
            $inherit, [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow)
        [void]$acl.AddAccessRule($rule)
    }
    foreach ($principal in @($iisUsers, $authenticatedUsers)) {
        $readRule = [Security.AccessControl.FileSystemAccessRule]::new(
            $principal, [Security.AccessControl.FileSystemRights]::ReadAndExecute,
            $inherit, [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow)
        [void]$acl.AddAccessRule($readRule)
    }
    if ($Directory) { [IO.Directory]::SetAccessControl($Path, $acl) }
    else { [IO.File]::SetAccessControl($Path, $acl) }
}

function Stage-Bootstrap([string]$SourcePath) {
    $hash = (Get-FileHash -LiteralPath $SourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $root = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'Datadog\Installer\iis-rum-bootstrap'
    $directory = Join-Path $root $hash
    foreach ($path in @($root, $directory)) {
        if (Test-Path -LiteralPath $path) {
            if (((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "IIS bootstrap staging path is a reparse point: $path"
            }
        }
        else { [void](New-Item -ItemType Directory -Path $path) }
        Protect-BootstrapPath $path $true
    }
    $destination = Join-Path $directory 'ddiisrum-bootstrap.dll'
    if (-not (Test-Path -LiteralPath $destination)) {
        $temporary = Join-Path $directory "$([guid]::NewGuid().ToString('N')).tmp"
        try {
            Copy-Item -LiteralPath $SourcePath -Destination $temporary
            if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
                throw 'IIS bootstrap copy hash mismatch.'
            }
            Move-Item -LiteralPath $temporary -Destination $destination
        }
        finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        }
    }
    if (((Get-Item -LiteralPath $destination).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
        throw 'Durable IIS bootstrap DLL differs from its content-addressed path.'
    }
    Protect-BootstrapPath $destination $false
    return $destination
}

function Test-SchemaCompatibility([string]$Path) {
    $document = New-Object System.Xml.XmlDocument
    $document.Load($Path)
    $rumSections = $document.SelectNodes('/configSchema/sectionSchema[@name="system.webServer/datadogRum"]')
    if ($rumSections.Count -ne 1) {
        throw "IIS RUM schema must define exactly one system.webServer/datadogRum section: $Path"
    }
    $rum = $rumSections[0]
    $enabled = $rum.SelectSingleNode('attribute[@name="enabled"]')
    $version = $rum.SelectSingleNode('attribute[@name="version"]')
    if ($null -eq $enabled -or $enabled.GetAttribute('type') -ne 'bool' -or
        $null -eq $version -or $version.GetAttribute('type') -ne 'int') {
        throw "IIS RUM schema has incompatible enabled or version attributes: $Path"
    }
    $collections = $rum.SelectNodes('collection')
    if ($collections.Count -ne 1) {
        throw "IIS RUM schema must define exactly one option collection: $Path"
    }
    $collection = $collections[0]
    if ($collection.GetAttribute('addElement') -ne 'option' -or
        $collection.GetAttribute('removeElement') -ne 'remove' -or
        $collection.GetAttribute('clearElement') -ne 'clear' -or
        $collection.GetAttribute('allowDuplicates') -ne 'false') {
        throw "IIS RUM schema has incompatible option collection directives: $Path"
    }
    $name = $collection.SelectSingleNode('attribute[@name="name"]')
    $value = $collection.SelectSingleNode('attribute[@name="value"]')
    if ($null -eq $name -or $name.GetAttribute('type') -ne 'string' -or
        $name.GetAttribute('isUniqueKey') -ne 'true' -or
        $name.GetAttribute('required') -ne 'true' -or
        $name.GetAttribute('validationType') -ne 'nonEmptyString' -or
        $null -eq $value -or $value.GetAttribute('type') -ne 'string' -or
        $value.GetAttribute('required') -ne 'true' -or
        $value.GetAttribute('validationType') -ne 'nonEmptyString') {
        throw "IIS RUM schema has incompatible option name or value attributes: $Path"
    }
    $serverSections = $document.SelectNodes('/configSchema/sectionSchema[@name="system.applicationHost/datadogRumServer"]')
    if ($serverSections.Count -gt 1) {
        throw "IIS RUM schema defines duplicate datadogRumServer sections: $Path"
    }
    if ($serverSections.Count -eq 1) {
        $logLevel = $serverSections[0].SelectSingleNode('attribute[@name="logLevel"]')
        if ($null -eq $logLevel -or $logLevel.GetAttribute('type') -ne 'enum') {
            throw "IIS RUM schema has incompatible datadogRumServer logLevel: $Path"
        }
    }
    return $serverSections.Count -eq 1
}

function Assert-Section($Group, [string]$Name, [string]$AllowDefinition, [string]$OverrideMode) {
    $section = $Group.Sections[$Name]
    if ($null -eq $section) {
        $section = $Group.Sections.Add($Name)
        $section.AllowDefinition = $AllowDefinition
        $section.OverrideModeDefault = $OverrideMode
        return $true
    }
    if ($section.AllowDefinition -ne $AllowDefinition -or
        $section.OverrideModeDefault -ne $OverrideMode) {
        throw "Existing IIS section '$Name' has incompatible settings."
    }
    return $false
}

function Assert-NoRootExclusion([string]$ConfigFile, [string]$Name) {
    $xml = New-Object System.Xml.XmlDocument
    $xml.Load($ConfigFile)
    foreach ($node in $xml.SelectNodes('/configuration/system.webServer/modules | /configuration/location[@path=""]/system.webServer/modules')) {
        if ($null -ne $node.SelectSingleNode('clear')) {
            throw 'Root IIS modules contain <clear>; preserving existing inheritance policy.'
        }
        foreach ($remove in $node.SelectNodes('remove')) {
            if ($remove.GetAttribute('name') -eq $Name) {
                throw "IIS module '$Name' is explicitly removed at the server root."
            }
        }
    }
}

function Assert-OwnedRootEntryUnchanged([string]$ConfigFile, [string]$Name) {
    $xml = New-Object System.Xml.XmlDocument
    $xml.Load($ConfigFile)
    $selector = '/configuration/system.webServer/modules/add[@name="' + $Name +
        '"] | /configuration/location[@path=""]/system.webServer/modules/add[@name="' + $Name + '"]'
    $entries = $xml.SelectNodes($selector)
    if ($entries.Count -ne 1 -or $entries[0].Attributes.Count -ne 2 -or
        $entries[0].GetAttribute('preCondition') -ne 'bitness64' -or
        $entries[0].HasChildNodes) {
        throw "Owned IIS root module '$Name' changed since installation; preserving it."
    }
}

function Assert-OwnedGlobalEntryUnchanged([string]$ConfigFile, [string]$Name, [string]$OwnedPath) {
    $xml = New-Object System.Xml.XmlDocument
    $xml.Load($ConfigFile)
    $entries = $xml.SelectNodes('/configuration/system.webServer/globalModules/add[@name="' + $Name + '"]')
    if ($entries.Count -ne 1 -or $entries[0].Attributes.Count -ne 3 -or
        $entries[0].GetAttribute('preCondition') -ne 'bitness64' -or
        -not [string]::Equals($entries[0].GetAttribute('image'), $OwnedPath,
            [StringComparison]::OrdinalIgnoreCase) -or
        $entries[0].HasChildNodes) {
        throw "Owned IIS global module '$Name' changed since installation; preserving it."
    }
}

function Assert-BootstrapOwnedPath([string]$OwnedPath) {
    $path = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($OwnedPath))
    $hash = [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($path))
    $root = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'Datadog\Installer\iis-rum-bootstrap'
    $expected = Join-Path (Join-Path $root $hash) 'ddiisrum-bootstrap.dll'
    if ($hash -cnotmatch '^[0-9a-f]{64}$' -or -not (Same-Path $path $expected)) {
        throw 'Existing IIS bootstrap ownership marker does not point to a durable bootstrap path.'
    }
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $hash) {
            throw 'Existing durable IIS bootstrap DLL differs from its ownership marker.'
        }
        Assert-CompatibleModule $path 'DatadogIisRumBootstrapVersion'
    }
}

function New-IisManager {
    $redirectionFile = Join-Path $env:windir 'System32\inetsrv\config\redirection.config'
    if (Test-Path -LiteralPath $redirectionFile) {
        $redirection = New-Object System.Xml.XmlDocument
        $redirection.Load($redirectionFile)
        if ($redirection.DocumentElement.GetAttribute('enabled') -eq 'true') {
            throw 'IIS Shared Configuration is enabled; install with the shared-config owner.'
        }
    }
    $assemblyPath = Join-Path $env:windir 'System32\inetsrv\Microsoft.Web.Administration.dll'
    if (-not (Test-Path -LiteralPath $assemblyPath -PathType Leaf)) {
        throw 'Microsoft.Web.Administration.dll was not found; IIS is required.'
    }
    Add-Type -Path $assemblyPath
    return New-Object Microsoft.Web.Administration.ServerManager
}

function Get-IisSections($Manager) {
    $configuration = $Manager.GetApplicationHostConfiguration()
    $webGroup = $configuration.RootSectionGroup.SectionGroups['system.webServer']
    $hostGroup = $configuration.RootSectionGroup.SectionGroups['system.applicationHost']
    if ($null -eq $webGroup -or $null -eq $hostGroup) {
        throw 'Required IIS section group is missing.'
    }
    return @{
        WebGroup = $webGroup
        HostGroup = $hostGroup
        Global = $configuration.GetSection('system.webServer/globalModules').GetCollection()
        Root = $configuration.GetSection('system.webServer/modules').GetCollection()
    }
}

$ownershipKey = 'SOFTWARE\Datadog\AutoInject\IIS RUM'
$defaultKey = 'SOFTWARE\Datadog\IIS RUM'
$moduleMarker = "$ModuleName.ModulePath"
$rootMarker = "$ModuleName.RootEntry"
$configMarker = "$ModuleName.DefaultConfigSha256"
$configFile = Join-Path $env:windir 'System32\inetsrv\config\applicationHost.config'
$schemaDestination = Join-Path $env:windir 'System32\inetsrv\config\schema\RUM_schema.xml'

if ($Bootstrap -and $ModuleName -ne 'DatadogIisRumAutoInject') {
    throw 'Bootstrap registration requires module name DatadogIisRumAutoInject.'
}
if (-not $Bootstrap -and $ModuleName -eq 'DatadogIisRumAutoInject') {
    throw 'DatadogIisRumAutoInject is reserved for automatic bootstrap registration.'
}

if ($Action -eq 'install') {
    if ([string]::IsNullOrWhiteSpace($ModulePath) -or [string]::IsNullOrWhiteSpace($SchemaPath)) {
        throw 'Install requires -ModulePath and -SchemaPath.'
    }
    $ModulePath = [IO.Path]::GetFullPath($ModulePath)
    $SchemaPath = [IO.Path]::GetFullPath($SchemaPath)
    if (-not (Test-Path -LiteralPath $ModulePath -PathType Leaf)) { throw "Module DLL not found: $ModulePath" }
    if (-not (Test-Path -LiteralPath $SchemaPath -PathType Leaf)) { throw "IIS schema not found: $SchemaPath" }
    if ([IO.Path]::GetExtension($ModulePath) -ine '.dll') { throw 'ModulePath must be a DLL.' }
    if ($Bootstrap) {
        Assert-CompatibleModule $ModulePath 'DatadogIisRumBootstrapVersion'
        $ModulePath = Stage-Bootstrap $ModulePath
        Assert-CompatibleModule $ModulePath 'DatadogIisRumBootstrapVersion'
    }
    else {
        Assert-CompatibleModule $ModulePath 'DatadogRumAutoConfigVersion'
    }
    $sourceHasServerSchema = Test-SchemaCompatibility $SchemaPath
    if (-not (Test-Path -LiteralPath (Split-Path -Parent $schemaDestination) -PathType Container)) {
        throw 'IIS schema directory was not found.'
    }
    $schemaCreated = -not (Test-Path -LiteralPath $schemaDestination -PathType Leaf)
    $hasServerSchema = $sourceHasServerSchema
    if (-not $schemaCreated) {
        $hasServerSchema = Test-SchemaCompatibility $schemaDestination
    }

    Assert-NoRootExclusion $configFile $ModuleName
    $manager = $null
    try {
        if ($schemaCreated) { Copy-Item -LiteralPath $SchemaPath -Destination $schemaDestination }
        $manager = New-IisManager
        $sections = Get-IisSections $manager
        $changed = Assert-Section $sections.WebGroup 'datadogRum' 'Everywhere' 'Allow'
        if ($hasServerSchema) {
            $changed = (Assert-Section $sections.HostGroup 'datadogRumServer' 'AppHostOnly' 'Deny') -or $changed
        }

        $globalEntry = Get-Entry $sections.Global $ModuleName
        $rootEntry = Get-Entry $sections.Root $ModuleName
        $createdGlobal = $null -eq $globalEntry
        $createdRoot = $null -eq $rootEntry
        foreach ($candidate in $sections.Global) {
            if ($candidate.ElementTagName -ne 'add' -or
                [string]$candidate.GetAttributeValue('name') -eq $ModuleName) { continue }
            if (Same-Path ([string]$candidate.GetAttributeValue('image')) $ModulePath) {
                throw "The IIS RUM DLL is already registered as '$($candidate.GetAttributeValue('name'))'."
            }
        }
        if ($ModuleName -eq 'DDRumModule' -and $createdGlobal) {
            foreach ($candidate in $sections.Global) {
                if ($candidate.ElementTagName -ne 'add') { continue }
                $imageName = [IO.Path]::GetFileName([string]$candidate.GetAttributeValue('image'))
                if ($imageName -ieq 'injector_IIS.dll' -or $imageName -ieq 'iis_injector.dll') {
                    throw "IIS RUM is already registered as '$($candidate.GetAttributeValue('name'))'."
                }
            }
        }
        $oldModuleMarker = Get-Value $ownershipKey $moduleMarker
        $oldRootMarker = Get-Value $ownershipKey $rootMarker
        if ($Bootstrap) {
            if (($createdGlobal -and $null -ne $oldModuleMarker) -or
                (-not $createdGlobal -and $null -eq $oldModuleMarker)) {
                throw "IIS bootstrap global registration '$ModuleName' is not owned by this installer."
            }
            if (($createdRoot -and $null -ne $oldRootMarker) -or
                (-not $createdRoot -and $null -eq $oldRootMarker)) {
                throw "IIS bootstrap root registration '$ModuleName' is not owned by this installer."
            }
            if (-not $createdGlobal) {
                Assert-BootstrapOwnedPath $oldModuleMarker
                Assert-OwnedGlobalEntryUnchanged $configFile $ModuleName $oldModuleMarker
                if (-not (Same-Path ([string]$globalEntry.GetAttributeValue('image')) $oldModuleMarker)) {
                    throw "IIS bootstrap global registration '$ModuleName' changed since installation."
                }
            }
            if (-not $createdRoot) {
                Assert-OwnedRootEntryUnchanged $configFile $ModuleName
                if (-not (Same-Path $oldRootMarker $oldModuleMarker)) {
                    throw "IIS bootstrap root registration '$ModuleName' changed since installation."
                }
            }
        }
        $upgradeGlobal = $Bootstrap -and -not $createdGlobal -and
            -not (Same-Path ([string]$globalEntry.GetAttributeValue('image')) $ModulePath)
        if (-not $createdGlobal -and -not $upgradeGlobal -and
            -not (Same-Path ([string]$globalEntry.GetAttributeValue('image')) $ModulePath)) {
            throw "IIS module '$ModuleName' already points to another DLL."
        }
        if ($createdGlobal -and -not $createdRoot) {
            throw "IIS module '$ModuleName' is present without a matching global module."
        }
        if (-not $createdGlobal -and [string]$globalEntry.GetAttributeValue('preCondition') -ne 'bitness64') {
            throw "IIS module '$ModuleName' needs a bitness64 global precondition."
        }
        if (-not $createdRoot -and [string]$rootEntry.GetAttributeValue('preCondition') -notin @('', 'bitness64')) {
            throw "IIS module '$ModuleName' has an incompatible root precondition."
        }

        $newConfig = $null
        if ($ConfigPath) {
            $newConfig = [IO.File]::ReadAllText([IO.Path]::GetFullPath($ConfigPath))
            if (($newConfig.Length + 1) * 2 -gt 65536) {
                throw 'RUM default JSON exceeds the module registry value limit of 65536 bytes.'
            }
            try { $parsed = ConvertFrom-Json -InputObject $newConfig -ErrorAction Stop }
            catch { throw 'Invalid RUM default JSON.' }
            if ($parsed -isnot [pscustomobject] -or
                $parsed.enabled -isnot [bool] -or
                ($parsed.version -isnot [int] -and $parsed.version -isnot [long]) -or
                $parsed.version -lt 5 -or $parsed.version -gt 1000 -or
                $parsed.options -isnot [pscustomobject] -or
                $parsed.options.applicationId -isnot [string] -or
                $parsed.options.clientToken -isnot [string] -or
                [string]::IsNullOrWhiteSpace($parsed.options.applicationId) -or
                [string]::IsNullOrWhiteSpace($parsed.options.clientToken)) {
                throw 'RUM default JSON needs boolean enabled, integer version 5..1000, and nonempty applicationId and clientToken strings.'
            }
            foreach ($option in $parsed.options.PSObject.Properties) {
                if ([string]::IsNullOrWhiteSpace($option.Name) -or
                    $option.Value -isnot [string] -or [string]::IsNullOrWhiteSpace($option.Value)) {
                    throw "RUM default option '$($option.Name)' must be a nonempty string."
                }
            }
            $currentConfig = Get-Value $defaultKey 'DefaultConfig'
            $currentMarker = Get-Value $ownershipKey $configMarker
            if ($null -ne $currentConfig -and
                (Get-ValueKind $defaultKey 'DefaultConfig') -ne [Microsoft.Win32.RegistryValueKind]::String) {
                throw 'Existing IIS RUM DefaultConfig is not REG_SZ; leaving it unchanged.'
            }
            if ($null -ne $currentConfig -and $currentConfig -cne $newConfig -and
                $currentMarker -ne (Get-Hash $currentConfig)) {
                throw 'Existing IIS RUM DefaultConfig is unmanaged; leaving it unchanged.'
            }
        }

        if (-not $Bootstrap -and $null -ne $oldModuleMarker -and
            -not (Same-Path $oldModuleMarker $ModulePath)) {
            throw "Existing ownership marker for '$ModuleName' points to another DLL."
        }
        $oldConfig = Get-Value $defaultKey 'DefaultConfig'
        $oldConfigMarker = Get-Value $ownershipKey $configMarker
        $configWritten = $false
        if ($createdGlobal) {
            $globalEntry = $sections.Global.CreateElement('add')
            $globalEntry.SetAttributeValue('name', $ModuleName)
            $globalEntry.SetAttributeValue('image', $ModulePath)
            $globalEntry.SetAttributeValue('preCondition', 'bitness64')
            $sections.Global.Add($globalEntry)
            $changed = $true
        }
        elseif ($upgradeGlobal) {
            $globalEntry.SetAttributeValue('image', $ModulePath)
            $changed = $true
        }
        if ($createdRoot) {
            $rootEntry = $sections.Root.CreateElement('add')
            $rootEntry.SetAttributeValue('name', $ModuleName)
            $rootEntry.SetAttributeValue('preCondition', 'bitness64')
            $sections.Root.Add($rootEntry)
            $changed = $true
        }
        try {
            $moduleMarkerWritten = $false
            $rootMarkerWritten = $false
            if ($createdGlobal -or $upgradeGlobal) {
                Set-Value $ownershipKey $moduleMarker $ModulePath
                $moduleMarkerWritten = $true
            }
            if ($createdRoot -or ($upgradeGlobal -and -not $createdRoot)) {
                Set-Value $ownershipKey $rootMarker $ModulePath
                $rootMarkerWritten = $true
            }
            if ($null -ne $newConfig) {
                if ($oldConfig -cne $newConfig) {
                    Set-Value $defaultKey 'DefaultConfig' $newConfig
                    $configWritten = $true
                }
                if ($null -eq $oldConfig -or
                    ($null -ne $oldConfigMarker -and $oldConfigMarker -eq (Get-Hash $oldConfig))) {
                    Set-Value $ownershipKey $configMarker (Get-Hash $newConfig)
                }
            }
            if ($changed) { $manager.CommitChanges() }
        }
        catch {
            if ($schemaCreated -and (Test-Path -LiteralPath $schemaDestination)) {
                Remove-Item -LiteralPath $schemaDestination -Force
            }
            if ($moduleMarkerWritten) {
                if ($null -eq $oldModuleMarker) { Remove-Value $ownershipKey $moduleMarker }
                else { Set-Value $ownershipKey $moduleMarker $oldModuleMarker }
            }
            if ($rootMarkerWritten) {
                if ($null -eq $oldRootMarker) { Remove-Value $ownershipKey $rootMarker }
                else { Set-Value $ownershipKey $rootMarker $oldRootMarker }
            }
            if ($configWritten) {
                if ($null -eq $oldConfig) { Remove-Value $defaultKey 'DefaultConfig' }
                else { Set-Value $defaultKey 'DefaultConfig' $oldConfig }
            }
            if ($null -eq $oldConfigMarker) { Remove-Value $ownershipKey $configMarker }
            else { Set-Value $ownershipKey $configMarker $oldConfigMarker }
            throw
        }
    }
    catch {
        if ($schemaCreated -and (Test-Path -LiteralPath $schemaDestination)) {
            Remove-Item -LiteralPath $schemaDestination -Force
        }
        throw
    }
    finally { if ($null -ne $manager) { $manager.Dispose() } }
    Write-Output "IIS RUM module '$ModuleName' is registered for inherited applications."
}
else {
    $ownedRoot = Get-Value $ownershipKey $rootMarker
    if ($null -ne $ownedRoot) {
        Assert-OwnedRootEntryUnchanged $configFile $ModuleName
        $manager = New-IisManager
        try {
            $sections = Get-IisSections $manager
            $globalEntry = Get-Entry $sections.Global $ModuleName
            $rootEntry = Get-Entry $sections.Root $ModuleName
            if ($null -eq $globalEntry -or
                -not (Same-Path ([string]$globalEntry.GetAttributeValue('image')) $ownedRoot) -or
                [string]$globalEntry.GetAttributeValue('preCondition') -ne 'bitness64' -or
                $null -eq $rootEntry -or
                [string]$rootEntry.GetAttributeValue('preCondition') -ne 'bitness64') {
                throw "IIS module '$ModuleName' changed since installation; preserving its registration."
            }
            $sections.Root.Remove($rootEntry)
            $manager.CommitChanges()
            Remove-Value $ownershipKey $rootMarker
        }
        finally { $manager.Dispose() }
    }
    $oldConfig = Get-Value $defaultKey 'DefaultConfig'
    $ownedHash = Get-Value $ownershipKey $configMarker
    if ($null -ne $ownedHash) {
        if ($null -ne $oldConfig -and
            (Get-ValueKind $defaultKey 'DefaultConfig') -eq [Microsoft.Win32.RegistryValueKind]::String -and
            (Get-Hash $oldConfig) -eq $ownedHash) {
            Remove-Value $defaultKey 'DefaultConfig'
        }
        Remove-Value $ownershipKey $configMarker
    }
    # Existing web.config files may still depend on these schema and section definitions.
    Write-Output "Inherited IIS RUM activation '$ModuleName' was removed; global registration remains for manually configured applications."
}
