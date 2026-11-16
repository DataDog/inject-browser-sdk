# Automatic IIS RUM bootstrap bundle

This directory owns the Windows IIS registration lifecycle used by
`datadog-apm-inject`. The consumer stages the complete archive under its
`package/iis-rum` directory and calls the public hook:

```powershell
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass `
  -File <absolute-bundle-directory>\Invoke-IisRumLifecycle.ps1 `
  -Action install -ContractVersion 1
```

Use `-Action uninstall` for removal. The hook validates the v1 manifest and
assets before changing IIS, and succeeds without registering anything when IIS
is absent. It invokes the internal `Install-IisRum.ps1` with the reserved name
`DatadogIisRumAutoInject` and `-Bootstrap`. It takes no RUM credentials.
Uninstall removes inherited activation; it retains the durable DLL, schema,
and global registration because an application may still refer to them.

The archive root contains only:

- `manifest.json` with `contract_version: 1`, `architecture: "x64"`,
  `runtime_abi: 1`, and an explicit `bundle_version`;
- `Invoke-IisRumLifecycle.ps1` and internal `Install-IisRum.ps1`;
- `ddiisrum-bootstrap.dll` and `RUM_schema.xml`.

The versioned archive name is `iis-rum-bootstrap-<bundle_version>-x64.zip`.
The existing GitLab Windows build signs the DLL and both scripts with `dd-wcs`
when `SIGN_WINDOWS_DD_WCS=true`, then packages the signed files and emits the
archive plus `<archive>.sha256` in its `artifacts` directory. The GitHub IIS
workflow also produces an explicitly unsigned development artifact. A release
owner must hand the consumer a published signed archive URL and its exact
SHA256; no URL or checksum is assumed here. The consumer stages the whole
archive and checks its configured SHA256. Release provenance comes from the
signed producer build; the consumer does not independently check signatures.

To build directly from this checkout on Windows x64, configure and build
`injector_IIS` with CMake, then run:

```powershell
python injector_IIS\scripts\auto\package_bundle.py `
  --bootstrap-dll injector_IIS\build\bootstrap\ddiisrum-bootstrap.dll `
  --bundle-version 1.2.1-dev --out-dir bundle-out
```

The packaging script validates the DLL's x64 PE header and prints the version,
archive path, and SHA256. It does not sign files. The ABI header has one source
at `injector_IIS/src/iis_rum_runtime_abi.h`, shared by the optional runtime and
the bootstrap build. The schema source is `injector_IIS/resources/RUM_schema.xml`.

For a quick Windows x64 native check from an x64 Visual Studio developer shell:

```powershell
cmake -S injector_IIS\bootstrap -B bootstrap-build -G Ninja -DIIS_BUILD_TESTS=ON
cmake --build bootstrap-build -j 4
ctest --test-dir bootstrap-build --output-on-failure
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass `
  -File injector_IIS\test\lifecycle\Test-IisRumBundleContract.ps1
```

The bootstrap uses IIS native module notifications and loads the implementation
only from the active separate `datadog-rum-inject` package. See
[ARCHITECTURE.md](ARCHITECTURE.md) for package selection, lifetime, output
caching, and configuration semantics. The runtime package contains
`iis/injector_IIS.dll`; it is not part of this bootstrap archive.

The package lifecycle test is
[`Run-IisRumLifecycle.ps1`](../../test/lifecycle/Run-IisRumLifecycle.ps1).
It needs an isolated IIS host and explicit test root, extracted bundle path,
and matching optional runtime DLL path. It tests activation, withdrawal,
same-worker behavior, background traffic, and cleanup after withdrawal. The
read-only contract test is
[`Test-IisRumBundleContract.ps1`](../../test/lifecycle/Test-IisRumBundleContract.ps1).
On an isolated IIS test host with the documented fixture, run it as
Administrator with a fresh root and assets built from this checkout:

```powershell
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass `
  -File injector_IIS\test\lifecycle\Run-IisRumLifecycle.ps1 `
  -TestRoot C:\dd-iis-rum-split-live-<unique> `
  -BundlePath C:\path\to\extracted-bundle `
  -RumDllPath C:\path\to\build\iis_injector.dll
```

Initial bootstrap registration changes inherited IIS configuration and can
recycle existing workers. Later optional RUM package arrival and withdrawal
require no forced recycle. Switching to a different runtime target requires a
worker recycle before that version is used. Existing in-flight requests may
finish with their previous policy. Browser and proxy caches are outside IIS
output-cache control. This implementation targets x64 workers on Windows
Server 2016 or newer; 32-bit pools are excluded by `bitness64` preconditions.
