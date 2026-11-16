# Read-only schema validation. Run with the pinned source RUM_schema.xml path.
param([Parameter(Mandatory = $true)][string]$SourceSchemaPath)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$installer = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'scripts\auto\Install-IisRum.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Installer script has PowerShell parse errors.' }
$function = $ast.Find({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Test-SchemaCompatibility'
}, $true)
if ($null -eq $function) { throw 'Schema compatibility function was not found.' }
. ([scriptblock]::Create($function.Extent.Text))

if ((Test-SchemaCompatibility $SourceSchemaPath) -ne $true) {
    throw 'Pinned source schema lost datadogRumServer.'
}
$legacy = Join-Path $PSScriptRoot 'compatible-legacy-schema.xml'
if ((Test-SchemaCompatibility $legacy) -ne $false) {
    throw 'Compatible legacy schema with no server section was rejected.'
}
$bad = Join-Path $PSScriptRoot 'incompatible-schema.xml'
$rejected = $false
try { [void](Test-SchemaCompatibility $bad) }
catch { $rejected = $true }
if (-not $rejected) { throw 'Incompatible option value type was accepted.' }
Write-Output 'Schema compatibility checks passed.'
