# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

# Find the Windows build images this runner already has cached. The runner AMI
# pre-pulls them, so IIS jobs pinned to one skip a ~12 minute pull.
# Writes a dotenv file for scripts/bump-windows-build-image.sh with
# WINDOWS_PREWARMED_IMAGE=<repo>:<newest v* tag>@<digest> and/or
# WINDOWS_PREWARMED_LATEST=<repo>:latest@<digest>; the bump script maps the
# latter to a v* tag and keeps whichever is newer.
param(
    [string]$Repository = $env:WINDOWS_BUILD_IMAGE_REPO,
    [string]$Output = 'windows-build-image.env'
)
$ErrorActionPreference = 'Stop'
if (-not $Repository) { throw 'Set WINDOWS_BUILD_IMAGE_REPO or pass -Repository' }

function Get-PinnedImage([string]$Tag) {
    $repoDigests = docker image inspect --format '{{json .RepoDigests}}' "${Repository}:${Tag}"
    if ($LASTEXITCODE -ne 0) { throw "docker image inspect failed (exit $LASTEXITCODE)" }
    # Parenthesised: Windows PowerShell's ConvertFrom-Json emits an array as one item.
    $digest = ($repoDigests | ConvertFrom-Json) |
        Where-Object { $_ -like "${Repository}@sha256:*" } |
        Select-Object -First 1
    if (-not $digest) { throw "${Repository}:${Tag} has no registry digest" }
    "${Repository}:${Tag}@$($digest.Split('@')[1])"
}

$tags = docker images $Repository --format '{{.Tag}}'
if ($LASTEXITCODE -ne 0) { throw "docker images failed (exit $LASTEXITCODE)" }
Write-Host "runner=$env:CI_RUNNER_ID cached ${Repository} tags: $($tags -join ', ')"

# Tags are v<datadog-agent-buildimages pipeline ID>-<commit>; the ID orders them.
$versioned = $tags |
    Where-Object { $_ -match '^v\d+-[0-9a-f]+$' } |
    Sort-Object { [long]($_ -replace '^v(\d+)-.*$', '$1') } |
    Select-Object -Last 1

$lines = @()
if ($versioned) { $lines += "WINDOWS_PREWARMED_IMAGE=$(Get-PinnedImage $versioned)" }
# Some runner AMIs pre-pull only :latest, and a v* tag here may just be one an
# earlier job pulled, so report both.
if ($tags -contains 'latest') { $lines += "WINDOWS_PREWARMED_LATEST=$(Get-PinnedImage 'latest')" }
if (-not $lines) { throw "No ${Repository}:v* or :latest image is cached on this runner" }

$lines | ForEach-Object { Write-Host $_ }
# GitLab rejects a dotenv with the BOM that Windows PowerShell's Out-File adds.
[IO.File]::WriteAllText((Join-Path $PWD $Output), ($lines -join "`n") + "`n")
