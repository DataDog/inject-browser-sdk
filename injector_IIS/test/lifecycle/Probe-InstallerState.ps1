# Read-only snapshot for preflight rejection tests. Never prints registry values.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$config = Join-Path $env:windir 'System32\inetsrv\config\applicationHost.config'
$schema = Join-Path $env:windir 'System32\inetsrv\config\schema\RUM_schema.xml'
$base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine,
    [Microsoft.Win32.RegistryView]::Registry64)
try {
    $key = $base.OpenSubKey('SOFTWARE\Datadog\IIS RUM', $false)
    $value = $null
    if ($null -ne $key) {
        try { $value = $key.GetValue('DefaultConfig', $null, 'DoNotExpandEnvironmentNames') }
        finally { $key.Dispose() }
    }
    $defaultHash = $null
    if ($null -ne $value) {
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $bytes = [Text.Encoding]::UTF8.GetBytes([string]$value)
            $defaultHash = [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '')
        }
        finally { $sha.Dispose() }
    }
    [pscustomobject]@{
        ApplicationHostSha256 = (Get-FileHash -LiteralPath $config -Algorithm SHA256).Hash
        SchemaSha256 = (Get-FileHash -LiteralPath $schema -Algorithm SHA256).Hash
        DefaultConfigSha256 = $defaultHash
    } | ConvertTo-Json -Compress
}
finally { $base.Dispose() }
