# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

param([Parameter(Mandatory)] [string]$ArtifactsDir)
$ErrorActionPreference = "Stop"

$hashes = [ordered]@{}
foreach ($name in @("injector_IIS.dll", "RUMTools.ps1", "Datadog.RUM.psm1", "Datadog.RUM.psd1")) {
    $path = Join-Path $ArtifactsDir $name
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne "Valid" -or $signature.SignatureType -ne "Authenticode") {
        throw "Invalid embedded Authenticode signature on ${path}: $($signature.Status) ($($signature.StatusMessage))"
    }
    $hashes[$name] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# This manifest is carried with the payload from the trusted Windows signing
# job. The Linux packaging job checks every signable file against these hashes.
$manifest = Join-Path $ArtifactsDir "oci-signatures.json"
[System.IO.File]::WriteAllText($manifest, ($hashes | ConvertTo-Json), (New-Object System.Text.UTF8Encoding $false))
