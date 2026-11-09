RUM SDK Injector for Windows IIS
================================
A Native-Code module for IIS 7.0 and above, designed to inject the RUM Browser SDK on the fly into HTML pages.

## How it works?
[IIS module](https://learn.microsoft.com/en-us/iis/get-started/introduction-to-iis/iis-modules-overview) that
intercepts responses and injects the Datadog RUM SDK in the response body for responses that meet all conditions:
* Response code is `2xx` `4xx` or `5xx`
* HTTP Content-Type header begins with `text/html`
* HTTP `x-datadog-rum-injected` header is not present
* Body looks like an HTML document (see below)

The snippet only ever goes within `<head>`. Where is set by the `injectionStrategy` attribute of
the `datadogRum` section:

* `headCloseTag` (the default): right before the first `</head>` end tag, wherever it is in the
  body, if the first 1 KiB looks like an HTML document (it holds an `<html>`, `<meta>` or `<head>`
  tag). Only a `</head>` before body content (`<body>`, `<div>`…) counts, and none written in a
  comment, a script, a style or an attribute value. Nothing is held back but a `</head>` tag split
  across writes: the response streams through.
* `headOpenTag`: as early as possible. If the `<head>` tag is within the first 1 KiB, right after
  it, unless the head holds `<meta>` tags declaring the character encoding, `X-UA-Compatible` or a
  `Content-Security-Policy`: then right after the last of them, so a meta-delivered policy governs
  the snippet. Other `<meta>` tags (SEO, viewport, the tracer's `dd-trace-id`…) do not hold it
  back. Otherwise, if that 1 KiB looks like an HTML document (it holds an `<html>` or a `<meta>`
  tag), right after the first `<head>` tag that follows. A `<head>` tag after body content does
  not count: browsers ignore it. The first 1 KiB is held back, or up to 10 KiB while the head is
  still a run of `<meta>` and `<title>` tags past it.

```xml
<datadogRum enabled="true" injectionStrategy="headOpenTag">
```

Either way, a response that does not look like an HTML document is served untouched.

## Build/installation steps
1. Configure the project
```sh
cmake -B build -DCMAKE_BUILD_TYPE=Release .
```

To cross-compile from a Linux host for Windows:
```sh
cmake -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_TOOLCHAIN_FILE=../cmake/toolchain-x86_64-pc-windows-msvc.cmake .
```
2. Compile

```sh
cmake --build build -j
```

## Installation
1. On your target host, copy `injector_IIS.dll` and `injectbrowsersdk.dll` into `%windir%\System32\inetsrv`
    - both files can be found in the output directory from your build in step 2
2. Set the required system environment variables: `DD_APPLICATION_ID` and `DD_CLIENT_TOKEN`
    - the available configuration environment variables are listed in `src/ruminjector.h`
3. Open `IIS manager` > `Modules` > `Configure Native Modules` and add `injector_ISS.dll` as a module (you can give it whatever name you like)
4. Start your IIS server and inspect the resulting webpage to validate that the Datadog RUM script was injected
    - If any problems occur, for Debug builds you can check the windows event logs for details

## Upgrade steps
1. Stop the IIS server
2. Update DLL
3. Start the IIS server

## Uninstall steps
1. Stop the IIS server
2. Remove the DLLs
3. Open `%windir%\system32\inetsrv\config\applicationhost.config` and remove the module from  module the `<globalModules>` & `<modules>` sections\
4. Start the IIS server

## Installer
Native Windows installer with [WiX Toolset v3.11](https://wixtoolset.org/releases/v3.11/stable).

Build locally with Visual Studio C++ tools, CMake, Rust and WiX 3.x installed:

```powershell
.\injector_IIS\scripts\build-msi.ps1
# Optional local BUILD override; PatchVer remains a compatibility alias.
.\injector_IIS\scripts\build-msi.ps1 -BuildVer 123
```

`VERSION` holds the release version (`1.3.0`), which release-please updates in
its release PR; don't edit it by hand. The MSI ProductVersion and DLL file
version are `MAJ.MIN.BUILD.0`, where BUILD is `CI_PIPELINE_IID`, or `0` when
unset or empty. Windows Installer has no room for PATCH next to BUILD, so PATCH
only appears in the display version. Major/minor must be decimal values from 0
to 255; PATCH and BUILD must be from 0 to 65535. Invalid values fail before
building. Each new GitLab pipeline gets a greater IID, allowing upgrades within
the same release; retrying a job in the same pipeline keeps the same
ProductVersion. The build warns once the IID passes 60000: plan a bounded BUILD
counter before it exceeds 65535.

The installer display name is `MAJ.MIN.PATCH-dev.BUILD` for development builds.
CI sets `IIS_RELEASE_BUILD=1` on the release-please branch and `iis-*` tags,
which drops the suffix (`MAJ.MIN.PATCH`) without changing the numeric version.
The build checks the actual MSI ProductVersion, ProductName and DLL metadata
before publishing artifacts, and exports `injector_IIS.version` (numeric) and
`injector_IIS.release` (display) for E2E and release checks.

## Releases

Releases are cut by [release-please](https://github.com/googleapis/release-please)
from Conventional Commit PR titles (see [CONTRIBUTING.md](../CONTRIBUTING.md)).
The jobs live in [`.gitlab/release.yml`](../.gitlab/release.yml):

1. Every main push updates the release PR on
   `release-please--branches--main--components--iis`, with the next version in
   `VERSION` and the changelog in `CHANGELOG.md`.
2. That branch's pipeline builds the MSI in release mode and runs all IIS
   unit, integration and E2E tests on that tree, unsigned. **A green release
   PR means the tree is ready to release.**
3. Merging the release PR authorizes the release: main tags `iis-X.Y.Z` and
   creates a draft GitHub release.
4. The tag pipeline first checks that the tag commit is the squash merge of
   the release PR with the same tree as the PR's head (GitHub API), and that
   GitLab has a successful release branch pipeline for that head. Otherwise it
   stops: a release PR merged before its pipeline passed on its final commit,
   or while out of date with main, is never built.
   It then builds that tree in release mode and signs the DLL,
   PowerShell modules and MSI with `windows-code-signer` (CI Identity
   credentials); the build fails unless every signature is valid. It runs the
   unit and integration tests, which install the signed MSI on a Windows CI
   runner, and E2E with signature checks, against those signed artifacts.
   Only then does it publish exactly those bytes. First
   `s3://rum-auto-instrumentation/iis/X.Y.Z/x64/` is written; it is
   created once and never replaced. Then `iis/latest/x64/` moves forward,
   never back to an older release. Both are read back over the public URL.
   Last, the MSI and `SHA256SUMS` go to the GitHub release, which is then
   published.

Only that tag pipeline signs. Every other build, including the release PR's,
is unsigned, so their E2E runs skip the signature check. If a publish job fails,
retry it in the same tag pipeline: it reuses the signed artifacts, and
republishing the same bytes is a no-op.

Pull requests that change the release path also build in release mode, run
`injector_IIS/test/release_tests/test_release_scripts.sh` (the release scripts
against stubbed `aws`, `curl` and `gh`), and dry-run release-please against
the PR branch.

# Powershell Module

The provided Powershell module creates or updates the Datadog RUM configuration in an IIS site's web.config or applicationHost.config
through `New-Datadog-RUMConfiguration`. It enables automatic injection of the Datadog RUM SDK into HTML pages served by the specified IIS site or application.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| **IISSite** | String | Yes | Specifies the Windows IIS site name for which Datadog RUM should be configured. |
| **ApplicationId** | String | Yes | RUM Application ID. Defined in the Datadog UI. |
| **ClientToken** | String | Yes | Datadog Client Token. Defined in the Datadog UI. |
| **DatadogSite** | String | No | Datadog site identifier for your organization (e.g., datadoghq.com or datadoghq.eu). |
| **SessionSampleRate** | Int32 | No | Percentage of sessions to track (between 0-100). |
| **SessionReplaySampleRate** | Int32 | No | Percentage of tracked sessions to capture (between 0-100). |
| **RemoteConfigurationId** | String | No | Remote configuration ID. Defined in the Datadog UI. |
| **Service** | String | No | Service name. |
