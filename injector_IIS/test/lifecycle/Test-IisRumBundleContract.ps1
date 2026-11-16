# Read-only contract checks using a fake Windows directory and a stub installer.
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$source = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'scripts\auto'
$root = Join-Path ([IO.Path]::GetTempPath()) ('iis-rum-contract-' + [guid]::NewGuid().ToString('N'))
$bundle = Join-Path $root 'bundle'
$fakeWindows = Join-Path $root 'windows'
$previousWindir = $env:windir
$calls = Join-Path $root 'calls.txt'

function Assert-Rejected([scriptblock]$Call) {
    $before = if (Test-Path -LiteralPath $calls) { @(Get-Content -LiteralPath $calls).Count } else { 0 }
    $rejected = $false
    try { & $Call | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw 'Invalid bundle invocation was accepted.' }
    $after = if (Test-Path -LiteralPath $calls) { @(Get-Content -LiteralPath $calls).Count } else { 0 }
    if ($after -ne $before) { throw 'Invalid bundle invocation reached the installer.' }
}

try {
    [void](New-Item -ItemType Directory -Path $bundle, $fakeWindows -Force)
    Copy-Item -LiteralPath (Join-Path $source 'Invoke-IisRumLifecycle.ps1') -Destination $bundle
    Copy-Item -LiteralPath (Join-Path (Split-Path -Parent (Split-Path -Parent $source)) 'resources\RUM_schema.xml') -Destination $bundle
    Set-Content -LiteralPath (Join-Path $bundle 'ddiisrum-bootstrap.dll') -Value 'asset'
    Set-Content -LiteralPath (Join-Path $bundle 'Install-IisRum.ps1') -Value @'
param([string]$Action, [string]$ModuleName, [switch]$Bootstrap,
      [string]$ModulePath, [string]$SchemaPath)
if ($ModuleName -ne 'DatadogIisRumAutoInject' -or -not $Bootstrap) { throw 'Unexpected registration.' }
if ($Action -eq 'install' -and (-not $ModulePath -or -not $SchemaPath)) { throw 'Missing assets.' }
if ($Action -eq 'uninstall' -and ($ModulePath -or $SchemaPath)) { throw 'Unexpected uninstall assets.' }
Add-Content -LiteralPath $env:IIS_RUM_TEST_CALLS -Value $Action
'@
    $env:windir = $fakeWindows
    $env:IIS_RUM_TEST_CALLS = $calls
    $manifest = Join-Path $bundle 'manifest.json'
    $valid = '{"contract_version":1,"architecture":"x64","runtime_abi":1,"bundle_version":"1.2.1-dev"}'
    Set-Content -LiteralPath $manifest -Value $valid -Encoding UTF8
    $hook = Join-Path $bundle 'Invoke-IisRumLifecycle.ps1'
    Assert-Rejected { & $hook -Action install -ContractVersion 2 }
    Set-Content -LiteralPath $manifest -Value ($valid.Replace('"runtime_abi":1', '"runtime_abi":2')) -Encoding UTF8
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value ($valid.Replace('"architecture":"x64"', '"architecture":"x86"')) -Encoding UTF8
    Assert-Rejected { & $hook -Action uninstall -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value ($valid.Replace('"bundle_version":"1.2.1-dev"', '"bundle_version":""')) -Encoding UTF8
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value ($valid.Replace('"contract_version":1', '"contract_version":"1"')) -Encoding UTF8
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value ($valid.TrimEnd('}') + ',"unexpected":true}') -Encoding UTF8
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value '{bad json' -Encoding UTF8
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath $manifest -Value $valid -Encoding UTF8
    Remove-Item -LiteralPath (Join-Path $bundle 'ddiisrum-bootstrap.dll')
    Assert-Rejected { & $hook -Action uninstall -ContractVersion 1 }
    Set-Content -LiteralPath (Join-Path $bundle 'ddiisrum-bootstrap.dll') -Value 'asset'
    & $hook -Action install -ContractVersion 1 | Out-Null
    if (Test-Path -LiteralPath $calls) { throw 'No-IIS path invoked the installer.' }

    $iis = Join-Path $fakeWindows 'System32\inetsrv'
    [void](New-Item -ItemType Directory -Path (Join-Path $iis 'config') -Force)
    Set-Content -LiteralPath (Join-Path $iis 'Microsoft.Web.Administration.dll') -Value 'stub'
    & $hook -Action install -ContractVersion 1 | Out-Null
    if (Test-Path -LiteralPath $calls) { throw 'Management tools alone invoked the installer.' }
    Remove-Item -LiteralPath (Join-Path $iis 'Microsoft.Web.Administration.dll')
    Set-Content -LiteralPath (Join-Path $iis 'config\applicationHost.config') -Value 'stub'
    Assert-Rejected { & $hook -Action install -ContractVersion 1 }
    Set-Content -LiteralPath (Join-Path $iis 'Microsoft.Web.Administration.dll') -Value 'stub'
    & $hook -Action install -ContractVersion 1 | Out-Null
    & $hook -Action uninstall -ContractVersion 1 | Out-Null
    $actions = @(Get-Content -LiteralPath $calls)
    if ($actions.Count -ne 2 -or $actions[0] -ne 'install' -or $actions[1] -ne 'uninstall') {
        throw "Wrong lifecycle calls: $($actions -join ',')"
    }
    Write-Output 'IIS RUM bundle contract checks passed.'
}
finally {
    $env:windir = $previousWindir
    Remove-Item Env:IIS_RUM_TEST_CALLS -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
