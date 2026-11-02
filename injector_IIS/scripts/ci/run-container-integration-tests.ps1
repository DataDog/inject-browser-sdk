# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

$ErrorActionPreference = "Stop"
& C:\inject-browser-sdk\injector_IIS\scripts\SetupIntegrationTestEnvironment.ps1
$ErrorActionPreference = "Continue"
pytest C:\inject-browser-sdk\tests\integration_tests\scenario -v
exit $LASTEXITCODE
