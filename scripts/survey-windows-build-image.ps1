# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

# Find the newest Windows build image this runner already has cached. The
# runner AMI pre-pulls it, so IIS jobs pinned to it skip a ~12 minute pull.
# Writes WINDOWS_PREWARMED_IMAGE=<repo>:<tag>@<digest> as a dotenv file for
# scripts/bump-windows-build-image.sh.
param(
    [string]$Repository = $env:WINDOWS_BUILD_IMAGE_REPO,
    [string]$Output = 'windows-build-image.env'
)
$ErrorActionPreference = 'Stop'
if (-not $Repository) { throw 'Set WINDOWS_BUILD_IMAGE_REPO or pass -Repository' }

$tags = docker images $Repository --format '{{.Tag}}'
if ($LASTEXITCODE -ne 0) { throw "docker images failed (exit $LASTEXITCODE)" }
Write-Host "runner=$env:CI_RUNNER_ID cached ${Repository} tags: $($tags -join ', ')"

# Tags are v<datadog-agent-buildimages pipeline ID>-<commit>; the ID orders them.
$tag = $tags |
    Where-Object { $_ -match '^v\d+-[0-9a-f]+$' } |
    Sort-Object { [long]($_ -replace '^v(\d+)-.*$', '$1') } |
    Select-Object -Last 1
if (-not $tag) { throw "No ${Repository}:v* image is cached on this runner" }

$repoDigests = docker image inspect --format '{{json .RepoDigests}}' "${Repository}:${tag}"
if ($LASTEXITCODE -ne 0) { throw "docker image inspect failed (exit $LASTEXITCODE)" }
# Parenthesised: Windows PowerShell's ConvertFrom-Json emits an array as one item.
$digest = ($repoDigests | ConvertFrom-Json) |
    Where-Object { $_ -like "${Repository}@sha256:*" } |
    Select-Object -First 1
if (-not $digest) { throw "${Repository}:${tag} has no registry digest" }

$image = "${Repository}:${tag}@$($digest.Split('@')[1])"
Write-Host "Newest pre-pulled image: $image"
# GitLab rejects a dotenv with the BOM that Windows PowerShell's Out-File adds.
[IO.File]::WriteAllText((Join-Path $PWD $Output), "WINDOWS_PREWARMED_IMAGE=$image`n")
