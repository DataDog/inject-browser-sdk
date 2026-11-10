# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

$ErrorActionPreference = "Stop"
& C:\inject-browser-sdk\injector_IIS\scripts\SetupIntegrationTestEnvironment.ps1

# Bypass the pytest.exe launcher and capture the Python process's exit code
# explicitly so a container failure can be distinguished from a test failure.
$pytest = Start-Process -FilePath python -ArgumentList @(
    "-m", "pytest", "C:\inject-browser-sdk\tests\integration_tests\scenario", "-v"
) -NoNewWindow -Wait -PassThru
$pytestExitCode = $pytest.ExitCode
Write-Host "pytest exited with code $pytestExitCode"
exit $pytestExitCode
