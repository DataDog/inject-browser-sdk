RUM SDK Injector for Windows IIS
================================
A Native-Code module for IIS 7.0 and above, designed to inject the RUM Browser SDK on the fly into HTML pages.

## How it works?
[IIS module](https://learn.microsoft.com/en-us/iis/get-started/introduction-to-iis/iis-modules-overview) that
intercepts responses and injects the Datadog RUM SDK in the response body for responses that meet all conditions:
* Response code is `2xx` `4xx` or `5xx`
* HTTP Content-Type header begins with `text/html`
* HTTP `x-datadog-rum-injected` header is not present
* Body contains `<head></head>` tag

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
2. That branch's pipeline builds the MSI in release mode, runs all IIS unit,
   integration and E2E tests on it, and stages the tested, unsigned MSI and DLL
   in MASS under `inject-browser-sdk/iis/candidates/<git tree>/`. **A green
   release PR means the artifact is ready.**
3. Merging the release PR authorizes the release: main tags `iis-X.Y.Z` and
   creates a draft GitHub release.
4. The tag pipeline does not recompile. It fetches the candidate for its
   tree and verifies the checksums, tag and version. It signs the candidate's
   DLL with `windows-code-signer` (CI Identity credentials), repackages it from
   the same tree with the candidate's version, and signs the MSI. Before
   publishing, it checks the Authenticode signatures, that the MSI contains the
   signed DLL and the candidate's files, and that its versions match the
   candidate. It then installs the signed MSI on a Windows CI runner and runs
   the integration tests against it. Finally, it publishes to
   `s3://rum-auto-instrumentation/iis/X.Y.Z/x64/` and `iis/latest/x64/`, reads
   both back over the public URL, and attaches the MSI and `SHA256SUMS` to the
   GitHub release before publishing it.

Only that tag pipeline signs. Branch builds, including the release PR's, are
unsigned, so E2E skips the signature check.

Merge the release PR only once its pipeline has passed for its latest commit,
with the branch up to date with main. Otherwise the merged tree has no tested
candidate and the tag pipeline stops before publishing anything. To recover,
push the tag's commit to the release branch
(`git push -f origin iis-X.Y.Z:refs/heads/release-please--branches--main--components--iis`),
wait for that pipeline to stage a candidate, then retry the tag pipeline's
`fetch_injector_IIS_release` job.

Pull requests that change the release path rehearse it without releasing. They
stage their tested build under `iis/rehearsals/`, build once more in release
mode, repackage and install-test it as the tag pipeline would (without
signing), publish to `s3://rum-auto-instrumentation/iis/rehearsal/<pipeline>/`,
check the GitHub release lookup read-only, and dry-run release-please against
the PR branch. `injector_IIS/test/release_tests/test_release_scripts.sh` covers
the release scripts locally with stubs.

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
