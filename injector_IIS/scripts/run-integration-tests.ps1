# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

<#
.SYNOPSIS
    Run integration tests against the locally configured IIS test environment.

.DESCRIPTION
    Assumes setup-integration-test-env.ps1 has already been run.
    Starts pytest via uv; the conftest.py fixture automatically starts the
    aiohttp upstream server on localhost:8090 for the duration of the session.

.EXAMPLE
    .\run-integration-tests.ps1
#>

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path $MyInvocation.MyCommand.Path -Parent
$RepoRoot  = Split-Path (Split-Path $ScriptDir -Parent) -Parent

# Activate xfail markers that account for IIS-specific behaviour
# (e.g. IIS hands a large proxied response to the module in several parts)
[System.Environment]::SetEnvironmentVariable("WEBSERVER-FLAVOR", "iis", "Process")

Write-Host "Running integration tests (upstream server started automatically by conftest.py)..."
# `--frozen` installs from the committed uv.lock without re-resolving against
# pyproject.toml. We avoid `--locked` because uv has a known false-positive on
# Windows where it reports the lockfile as stale even when `uv lock` produces no
# changes (astral-sh/uv issues #13614, #10683).
uv run --frozen --project "$RepoRoot\tests\integration_tests" `
    pytest "$RepoRoot\tests\integration_tests\scenario" -v

exit $LASTEXITCODE
