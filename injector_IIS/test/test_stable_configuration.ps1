# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

$ErrorActionPreference = "Stop"
. "$PSScriptRoot\..\scripts\test-stable-configuration.ps1"

$directory = Join-Path ([System.IO.Path]::GetTempPath()) "iis-rum-config-test-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $directory | Out-Null
try {
    $cases = 0
    foreach ($mode in @($false, $true)) {
        foreach ($existing in @($false, $true)) {
            foreach ($failureAt in @("none", "acl", "restart", "tests")) {
                if (-not $mode -and $failureAt -eq "acl") { continue }
                $path = Join-Path $directory "application_monitoring.yaml"
                $original = [byte[]]@(0, 255, 13, 10, 128, 42)
                if ($existing) {
                    [System.IO.File]::WriteAllBytes($path, $original)
                    if ($env:OS -eq "Windows_NT") { $originalAcl = (Get-Acl -LiteralPath $path).Sddl }
                }
                $script:restartCalls = 0
                $caught = $false
                try {
                    Invoke-WithTestStableConfiguration -StableConfig:$mode -Path $path -GrantReadAccess {
                        if ($failureAt -eq "acl") { throw "simulated ACL failure" }
                    } -RestartIis {
                        $script:restartCalls++
                        if ($failureAt -eq "restart" -and $script:restartCalls -eq 1) {
                            throw "simulated IIS restart failure"
                        }
                    } -RunTests {
                        if ((Test-Path -LiteralPath $path) -ne $mode) { throw "wrong fixture state" }
                        if ($mode -and -not ((Get-Content -LiteralPath $path -Raw).Contains("stable-config-app-id"))) {
                            throw "missing stable fixture"
                        }
                        if ($failureAt -eq "tests") { throw "simulated test failure" }
                    }
                } catch {
                    $caught = $true
                    if ($failureAt -eq "none") { throw }
                }
                if ($caught -ne ($failureAt -ne "none")) { throw "failure was not propagated" }
                if ((Test-Path -LiteralPath $path) -ne $existing) { throw "original existence was not restored" }
                if ($existing) {
                    if ([Convert]::ToBase64String([System.IO.File]::ReadAllBytes($path)) -ne [Convert]::ToBase64String($original)) {
                        throw "original bytes were changed"
                    }
                    if ($env:OS -eq "Windows_NT" -and (Get-Acl -LiteralPath $path).Sddl -ne $originalAcl) {
                        throw "original ACL was changed"
                    }
                    Remove-Item -LiteralPath $path
                }
                if (Get-ChildItem $directory) { throw "a backup or fixture was leaked" }
                $cases++
            }
        }
    }
    Write-Host "Passed $cases stable-config lifecycle cases."
} finally {
    Remove-Item -LiteralPath $directory -Recurse -Force
}
