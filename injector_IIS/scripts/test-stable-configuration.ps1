# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

function Invoke-WithTestStableConfiguration {
    param(
        [Parameter(Mandatory)] [scriptblock]$RunTests,
        [switch]$StableConfig,
        [string]$Path = "$env:ProgramData\Datadog\application_monitoring.yaml",
        [scriptblock]$RestartIis = {
            iisreset /restart | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "IIS restart failed: $LASTEXITCODE" }
        },
        [scriptblock]$GrantReadAccess = {
            param($FilePath)
            icacls $FilePath /grant "*S-1-5-11:(RX)" | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "Stable-config ACL update failed: $LASTEXITCODE" }
        }
    )

    $ErrorActionPreference = "Stop"

    $backupPath = "$Path.iis-rum-test-$([guid]::NewGuid().ToString('N')).bak"
    $originalMoved = $false
    $fixtureOwned = $false
    try {
        # A same-directory move preserves the original bytes and security
        # descriptor. Baseline tests temporarily hide the file, never delete it.
        if (Test-Path -LiteralPath $Path) {
            Move-Item -LiteralPath $Path -Destination $backupPath
            $originalMoved = $true
        }
        if ($StableConfig) {
            New-Item -ItemType Directory -Force -Path (Split-Path $Path -Parent) | Out-Null
            $yaml = @'
rules:
- selectors:
  - origin: language
    matches: ["browser-sdk"]
    operator: equals
  configuration:
    DD_RUM_APPLICATION_ID: stable-config-app-id
    DD_RUM_CLIENT_TOKEN: stable-config-client-token
    DD_RUM_SITE: datadoghq.com
    DD_RUM_MAJOR_VERSION: 6
    DD_RUM_SESSION_SAMPLE_RATE: 100
'@
            # CreateNew cannot overwrite a file created by another writer
            # after we moved the original aside.
            $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::CreateNew)
            $fixtureOwned = $true
            try {
                $bytes = (New-Object System.Text.UTF8Encoding $false).GetBytes($yaml)
                $stream.Write($bytes, 0, $bytes.Length)
            } finally {
                $stream.Dispose()
            }
            & $GrantReadAccess $Path
        }
        & $RestartIis
        & $RunTests
    } finally {
        try {
            if ($fixtureOwned -and (Test-Path -LiteralPath $Path)) {
                Remove-Item -LiteralPath $Path -Force
            }
        } finally {
            # Never overwrite an unexpected new file: if restoration fails,
            # the original remains intact at backupPath for recovery.
            try {
                if ($originalMoved) {
                    try {
                        Move-Item -LiteralPath $backupPath -Destination $Path
                    } catch {
                        Write-Warning "Original stable configuration remains at $backupPath"
                        throw "Cannot restore stable configuration; the original is preserved at ${backupPath}: $_"
                    }
                }
            } finally {
                & $RestartIis
            }
        }
    }
}
