# Public auto-inject bundle contract, version 1. This script is the only entry
# point the consumer calls; registration details remain owned by this package.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('install', 'uninstall')][string]$Action,
    [Parameter(Mandatory = $true)][int]$ContractVersion
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($ContractVersion -ne 1) { throw "Unsupported IIS RUM bundle contract: $ContractVersion" }
$manifestPath = Join-Path $PSScriptRoot 'manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw 'IIS RUM bundle manifest.json is missing.'
}
try {
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
}
catch { throw "Invalid IIS RUM bundle manifest: $($_.Exception.Message)" }
function Test-Integer($Value) { return ($Value -is [int] -or $Value -is [long]) }
if ($manifest -isnot [pscustomobject] -or
    @($manifest.PSObject.Properties).Count -ne 4 -or
    -not (Test-Integer $manifest.contract_version) -or $manifest.contract_version -ne 1 -or
    $manifest.architecture -cne 'x64' -or
    -not (Test-Integer $manifest.runtime_abi) -or $manifest.runtime_abi -ne 1 -or
    $manifest.bundle_version -isnot [string] -or
    $manifest.bundle_version -cnotmatch '^[0-9][0-9A-Za-z._+-]{0,127}$') {
    throw 'IIS RUM bundle manifest is incompatible with contract 1.'
}

$installer = Join-Path $PSScriptRoot 'Install-IisRum.ps1'
$bootstrap = Join-Path $PSScriptRoot 'ddiisrum-bootstrap.dll'
$schema = Join-Path $PSScriptRoot 'RUM_schema.xml'
foreach ($path in @($installer, $bootstrap, $schema)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "IIS RUM bundle asset is missing: $path"
    }
}

$iisRoot = Join-Path $env:windir 'System32\inetsrv'
$assembly = Join-Path $iisRoot 'Microsoft.Web.Administration.dll'
$applicationHost = Join-Path $iisRoot 'config\applicationHost.config'
if (-not (Test-Path -LiteralPath $applicationHost -PathType Leaf)) {
    Write-Output 'IIS is absent; no IIS RUM registration is needed.'
    return
}
if (-not (Test-Path -LiteralPath $assembly -PathType Leaf)) {
    throw 'IIS installation is incomplete; preserving registration state.'
}

$arguments = @{
    Action = $Action
    ModuleName = 'DatadogIisRumAutoInject'
    Bootstrap = $true
}
if ($Action -eq 'install') {
    $arguments.ModulePath = $bootstrap
    $arguments.SchemaPath = $schema
}
& $installer @arguments
