IIS injector OCI package
========================

`datadog-apm-library-iis-rum` delivers the IIS RUM injector through fleet
automation, so it can be installed and updated by remote configuration instead of
by hand-releasing an MSI. It is additive: the MSI keeps working unchanged.

## Building and publishing

Built by `injector_IIS_oci` from `injector_IIS_build`'s artifacts, never from a
fresh compile. The payload is Authenticode-signed on the Windows runner by
`iis.package`; the OCI job runs on Linux (`datadog-package` is a Linux binary) and
only ever *stages* already-signed files. Windows verifies each embedded signature
with `Get-AuthenticodeSignature` and records the SHA256 of each verified file in
`oci-signatures.json`. `iis.oci` requires this manifest from the Windows build job
and refuses any payload whose bytes differ, unless `--allow-unsigned` is passed;
CI never passes it. The manifest travels with CI artifacts and is not part of the
package layer.

```sh
cd injector_IIS
invoke iis.oci-version                     # X.Y.Z-1, refuses a -dev VERSION
invoke iis.oci                             # build the archive
invoke iis.oci --allow-unsigned            # ... from an unsigned local build
invoke iis.oci-push                        # publish (releases only)
```

CI builds pass `--allow-prerelease`, which yields `X.Y.Z-0.dev` so the packaging
path and `scripts/ci/assert_oci_package.py` run on every change. `iis.oci-push`
refuses such a version: there is no `apmPackageDefaultVersions` pin for `iis-rum`
agent-side, so `latest` is what an opted-in customer without a pin receives.

Registry naming splits: the ddbuild repository uses the package name, while the
public `oci://install.datadoghq.com/apm-library-iis-rum-package:<version>` name is
an S3 mirror produced by `datadog-package replicate-s3` from `agent-release-management` CI after the product is registered.

Internal pushes are development artifacts. Staging and production publication
must use `agent-release-management`, which replicates packages to public
registries and registers them in the signed Fleet catalog. A direct internal
push does not make this package available at the public URL. After product
onboarding, the release team recommends `dd-pkg promote` from the product CI
job, first targeting staging. It submits the promotion through artifact-gateway
to `agent-release-management`; the internal `datadog-package push` task here
does not perform that step.

Release references:

- [OCI Package lifecycle](https://datadoghq.atlassian.net/wiki/spaces/FAU/pages/3838182370)
- [Packaging workflow](https://datadoghq.atlassian.net/wiki/spaces/INJ/pages/6759383988)
- [Promote Agent packages](https://datadoghq.atlassian.net/wiki/pages/viewpage.action?pageId=6948160699)
- [Shared OCI publishing template proposal](https://datadoghq.atlassian.net/wiki/pages/viewpage.action?pageId=7126417898)


## Layer layout

```
datadog-apm-library-iis-rum/<version>/
  library/injector_IIS.dll                      the native IIS module
  installer/RUMTools.ps1                        Migrate-/Uninstall-Datadog-RUMConfiguration
  modules/Datadog.RUM/Datadog.RUM.psm1          folder name must equal the module
  modules/Datadog.RUM/Datadog.RUM.psd1            name for Import-Module to work
  schema/RUM_schema.xml                         -> %SystemRoot%\System32\inetsrv\config\schema
```

`library/` + `installer/` mirrors `datadog-apm-library-dotnet`'s on-disk layout.
`RUM_schema.xml` is the one unsigned file: it is neither a PE nor a PowerShell
script, so `dd-wcs` has nothing to attach a signature block to.

## Installing it today

```
DD_REMOTE_UPDATES=true
DD_APM_INSTRUMENTATION_ENABLED=iis
DD_APM_INSTRUMENTATION_LIBRARIES=iis-rum:<version>
```

The package is pre-registered in `pkg/fleet/installer/default_packages.go`
(`released: false`, `releasedWithRemoteUpdates: true`, condition
`apmLanguageExplicitlyEnabled`), and is in `ExplicitOnlyApmLibraries`, so `all`
will not pull it.

## Required agent-side work: the package is currently inert

There is **no `packagesHooks` entry** for `datadog-apm-library-iis-rum` in
`pkg/fleet/installer/packages/packages_windows.go`. Installation is therefore
extract-only: files land under
`C:\ProgramData\Datadog\Installer\packages\datadog-apm-library-iis-rum\<version>\`
with `stable`/`experiment` symlinks, and **nothing runs**. The module is not
registered with IIS, the schema is not installed, the configuration sections are
not declared, and the event source does not exist — so the injector does not
load. Until this is done, the package must be driven by hand after install.

Everything below is what the MSI does today (`injector_IIS_installer/Product.wxs`)
and what the hooks have to replicate.

### 1. Register the hooks

`pkg/fleet/installer/packages/packages_windows.go`:

```go
packagesHooks = map[string]hooks{
    ...
    "datadog-apm-library-iis-rum": apmLibraryIISRumPackage,
}
```

New file `pkg/fleet/installer/packages/apm_library_iis_rum_windows.go`, modelled
on `apm_library_dotnet_windows.go`:

```go
var apmLibraryIISRumPackage = hooks{
    postInstall:         postInstallAPMLibraryIISRum,
    preRemove:           preRemoveAPMLibraryIISRum,
    postStartExperiment: postStartExperimentAPMLibraryIISRum,
    preStopExperiment:   preStopExperimentAPMLibraryIISRum,
}
```

Resolve the install directory the same way the dotnet package does, so the
hooks operate on the concrete versioned directory rather than the symlink:

```go
installDir, err := filepath.EvalSymlinks(
    filepath.Join(paths.PackagesPath, "datadog-apm-library-iis-rum", "stable"))
```

### 2. What `postInstall` must do

In this order — the schema and the section declarations must exist before the
module loads, or `GetAdminSection` fails for every site:

1. **Install the schema.** Copy `schema/RUM_schema.xml` to
   `%SystemRoot%\System32\inetsrv\config\schema\RUM_schema.xml`.

2. **Declare both configuration sections** in
   `%SystemRoot%\System32\inetsrv\config\applicationHost.config`, idempotently
   (the MSI uses `util:XmlConfig` with a `VerifyPath`, so re-running is a no-op):

   - `configSections/sectionGroup[@name='system.webServer']` gains
     `<section name="datadogRum" overrideModeDefault="Allow" />` — `Allow` is
     required so a site's `web.config` can override it.
   - `configSections/sectionGroup[@name='system.applicationHost']` gains
     `<section name="datadogRumServer" overrideModeDefault="Deny" />`
     (AppHostOnly — this one is not delegatable).

3. **Register the native module**, pointing at the versioned `library/` path:

   ```
   %SystemRoot%\System32\inetsrv\appcmd.exe install module ^
       /name:DDRumModule ^
       /image:"<installDir>\library\injector_IIS.dll" ^
       /precondition:bitness64
   ```

   `/precondition:bitness64` matters: without it IIS tries to load the 64-bit DLL
   into 32-bit worker processes.

4. **Create the event source** so the module's logs are readable. The MSI's
   `util:EventSource` writes registry values under
   `HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Datadog-RUM-Instrumentation`:
   `EventMessageFile` = the DLL path, and `TypesSupported` = 7
   (`EVENTLOG_ERROR_TYPE | WARNING | INFORMATION`). The module registers this
   source name in `Logger`'s constructor (`src/logger.h`); without the registry
   entry every event is logged with unresolved message text.

Note on `EventMessageFile`: it must point at a DLL path that survives. Pointing
it at the versioned directory means it dangles after an upgrade removes that
version; pointing it at the `stable` symlink means it resolves to whatever is
current. Prefer the symlink.

### 3. What `preRemove` must do

Reverse of the above, tolerating each step already being undone:

1. `appcmd uninstall module DDRumModule`.
2. Remove the two `configSections` entries.
3. Remove the event-source registry key.
4. **Keep** `RUM_schema.xml`. Removing it while any site still has a
   `<datadogRum>` section makes `applicationHost.config` unparseable and takes
   IIS down — this repo already fixed exactly that bug for the MSI path (see
   commit `1370539`, "keep IIS schemas across uninstall").
5. Optionally run `installer/RUMTools.ps1 Uninstall-Datadog-RUMConfiguration` to
   strip per-site sections, as the MSI's `RemoveRUMConfig` custom action does.

### 4. Experiment hooks

`postStartExperiment` re-registers the module against the `experiment` symlink;
`preStopExperiment` re-registers it against `stable`. Both need an app-pool
recycle or `iisreset` to take effect, since IIS loads the DLL once per worker
process.

### 5. Other agent-side gaps

- **No `apmPackageDefaultVersions` pin** for `iis-rum`, so an opted-in customer
  without an explicit version gets `latest`.
- **Release-management onboarding must be completed** in
  `DataDog/agent-release-management`: the product, package flavor, component,
  and platform matrix must agree on `datadog-apm-library-iis-rum` and its public
  repository name `apm-library-iis-rum-package`. Source CI also needs permission
  to request promotion through artifact-gateway. Internal publishing in this
  repository prepares the package; release-management owns staging/prod
  replication and signed-catalog registration. The public URL only resolves
  after that promotion succeeds.
- **No e2e coverage** of the fleet install path.

### 6. File permissions on application_monitoring.yaml

Verified on a real IIS host: `C:\ProgramData\Datadog\application_monitoring.yaml`
as laid down by the agent is readable by `ddagentuser`, `SYSTEM` and
`Administrators` — but **not** by the IIS worker-process identity, which is
`ApplicationPoolIdentity` by default. The module then logs

    Ignoring unusable stable configuration: Stable config error:
    failed to read local config file: Access is denied. (os error 5)

and falls back to IIS-only behaviour, so the zero-touch story silently does not
work. Whichever component ends up owning the install (an agent-side hook, or the
agent installer when it writes the file) has to grant read access to the worker
identity.

## Configuration

Once loaded, the module needs no per-site step: it reads
`application_monitoring.yaml`. See the "Configuration sources" section of
`README.md`. Note that changes to that file require an app-pool recycle, and
that the file must be readable by the worker-process identity.
