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

`VERSION` holds the marketing version (`1.3-dev`). The MSI ProductVersion and
DLL file version are `MAJ.MIN.BUILD.0`, where BUILD is `CI_PIPELINE_IID`, or `0`
when unset or empty. Major/minor must be decimal values from 0 to 255; BUILD
must be from 0 to 65535. Invalid values fail before building. Each new GitLab
pipeline gets a greater IID, allowing upgrades within the same marketing
version; retrying a job in the same pipeline keeps the same ProductVersion.
Plan a bounded BUILD counter before the project's pipeline IID exceeds 65535.

The installer display name retains `-dev`; setting `IIS_RELEASE_BUILD=1`
removes the suffix without changing the numeric version. The build checks the
actual MSI ProductVersion and DLL metadata for equality before publishing
artifacts, and exports `injector_IIS.version` for install/upgrade E2E checks.

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
