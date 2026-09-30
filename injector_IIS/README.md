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

## Configuration sources

RUM settings come from two places, merged per key:

1. **Stable configuration** — `application_monitoring.yaml`, read once when the
   module loads. Both the local
   (`C:\ProgramData\Datadog\application_monitoring.yaml`) and the fleet-managed
   (`C:\ProgramData\Datadog\managed\datadog-agent\stable\application_monitoring.yaml`)
   file are read, with the fleet-managed one taking precedence. Entries are
   matched against `language: browser-sdk` and use the `DD_RUM_*` keys the agent
   installer writes (`DD_RUM_APPLICATION_ID`, `DD_RUM_CLIENT_TOKEN`,
   `DD_RUM_SITE`, `DD_RUM_ENABLED`, `DD_RUM_MAJOR_VERSION`, ...). This is what
   lets a host be configured centrally, with no per-site step.
2. **The per-site IIS section** — `system.webServer/datadogRum`, managed with
   `New-Datadog-RUMConfiguration`. Every option set here overrides the
   corresponding stable-configuration value for that site; options it doesn't
   mention are inherited.

A few things about the interaction are worth knowing:

* `enabled="false"` on the IIS section is the per-site escape hatch and outranks
  stable configuration. Conversely `DD_RUM_ENABLED: "false"` in
  `application_monitoring.yaml` switches injection off for every site that hasn't
  opted in through IIS.
* The Browser SDK major version is 6 unless something says otherwise. The agent
  installer does not write `DD_RUM_MAJOR_VERSION`, so set it in
  `application_monitoring.yaml` (or `version` on the IIS section) to pin a
  different one.
* **Changes to `application_monitoring.yaml` need an application-pool recycle**
  (or `iisreset`). The file lives outside IIS's own configuration tree, so IIS
  never notifies the module that it changed. Edits to the IIS section are picked
  up live, as before.
* **The file must be readable by the IIS worker-process identity.** IIS worker
  processes run as `ApplicationPoolIdentity` (or a custom pool identity), not as
  `ddagentuser`, and `C:\ProgramData\Datadog` is not readable by them by
  default. If the module logs `failed to read local config file: Access is
  denied`, grant read access — e.g.
  `icacls "%ProgramData%\Datadog\application_monitoring.yaml" /grant "*S-1-5-11:(RX)"`
  for all authenticated users, or the specific pool identity.

A broken or unreadable `application_monitoring.yaml` is logged and then ignored:
sites configured through IIS keep injecting.

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

Build from Windows with [WiX Toolset Build Tools](https://marketplace.visualstudio.com/items?itemName=WixToolset.WiXToolset). Build with
Visual Studio 2019 Community or:

```bash
MSBuild.exe injector_IIS\injector_IIS_installer\injector_IIS_installer.wixproj /p:InjectorDll=${pwd}\injector_IIS\build\iis_injector.dll
```

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
