# Automatic IIS RUM package lifecycle

This browser SDK repository owns the small, durable IIS loader and its
registration hook; Windows auto-inject distributes the signed bundle. The
separate RUM package owns the optional RUM implementation. IIS registers the loader through its public native
module interface; there are no private IIS hooks or application `web.config`
edits. The request/response integration uses Microsoft's
[native IIS interfaces](https://learn.microsoft.com/en-us/iis/web-development-reference/native-code-api-reference/ihttpresponse-interface).

```text
Windows injector installation
  -> durable ddiisrum-bootstrap.dll + IIS schema
  -> globalModules + inherited DatadogIisRumAutoInject activation

IIS request
  -> select a complete RUM package (experiment, then stable)
  -> lazily load its implementation through a versioned API
  -> read RUM defaults through the existing stable-config library
  -> apply effective IIS configuration over those defaults
  -> check package presence again before modifying the response
```

## Package contract

The separate optional package is `datadog-rum-inject`, under the Datadog
installer's `packages` directory. Each active `experiment` or `stable` directory
must contain an ordinary `version` file and `iis/injector_IIS.dll`. The DLL must
implement the versioned bootstrap API in `injector_IIS/src/iis_rum_runtime_abi.h`. Version directories
are immutable; the package manager publishes or withdraws active directory links.

Package absence prevents loading the RUM implementation. After a package has
been loaded, withdrawing it prevents later request/response injection after
withdrawal is observed; an already in-flight response may finish under its
previous policy. A
loaded DLL remains pinned until worker exit because IIS can still own context
objects whose cleanup code lives in that DLL. Removing the active link or version
canary must precede deletion of package payloads.

Installing a package after a worker has started activates it lazily. Restoring
the same package target can reuse its loaded runtime. Changing the selected
target to a different version requires worker recycling; the loader must not use
old code with another version's stored contexts. Promoting the same immutable
target from experiment to stable does not require a different runtime.

## Stable configuration

Credentials and automatic defaults come from the existing RUM library's stable
configuration reader. Its Windows defaults are:

- `C:\ProgramData\Datadog\application_monitoring.yaml`
- `C:\ProgramData\Datadog\managed\datadog-agent\stable\application_monitoring.yaml`

The existing library resolves process rules and fleet-over-local precedence.
Selectors see the worker process (`w3wp.exe`), its arguments and environment;
applications sharing a pool share these defaults. IIS does not introduce another
YAML parser. Explicit effective IIS values overlay that result for each
application. Collection clear/remove directives conservatively suppress all
automatic SDK option fallback while retaining the independently resolved enabled
flag and SDK major version. The automatic lifecycle does not require a registry
credential copy.

Stable configuration is read when the optional runtime initializes. Changing
the YAML after initialization requires worker recycling. Test-only path
overrides allow isolated validation without modifying the machine's real YAML.

## Ownership and manual settings

The loader's registration uses a name distinct from an existing manual
`DDRumModule`. It preserves existing module registrations, schemas, and manual
`datadogRum` settings. The RUM implementation retains effective IIS option
precedence, explicit disable/enable, and conservative collection removal handling.
An existing manual RUM module is independently owned and remains effective when
the automatic package is absent.

Bootstrap installation can recycle existing workers because it changes inherited
IIS registration. Later optional package arrival and withdrawal do not force a
recycle. Injector uninstallation removes its owned inherited activation. A durable loader,
global registration, and schema can remain to keep explicit application
references valid. RUM package removal never leaves IIS pointing to its removed
implementation DLL.

## Output caching

The live lifecycle test demonstrated that IIS can return a cached HTML response
without calling the loader after a package is published. The bootstrap therefore
disables IIS kernel and user output caching for responses matching the RUM
extension's `text/html` content-type filter, including while the optional package
is absent. The check runs at the public
[`UpdateRequestCache`](https://learn.microsoft.com/en-us/iis/web-development-reference/native-code-api-reference/chttpmodule-onupdaterequestcache-method)
notification with first priority, before IIS stores the response; a send-response
check also protects later output. This uses the public
[`DisableKernelCache`](https://learn.microsoft.com/en-us/iis/web-development-reference/native-code-api-reference/ihttpresponse-disablekernelcache-method)
and
[`DisableUserCache`](https://learn.microsoft.com/en-us/iis/web-development-reference/native-code-api-reference/ihttpcachepolicy-disableusercache-method)
methods. Other content types retain their existing policy. This has a performance
cost for applications relying on IIS HTML output caching. Browser and upstream
proxy caches are outside this mechanism; previously delivered pages cannot be
withdrawn.

## Lifetime requirements

New runtime creation and HTML injection are conditional on package presence.
An already loaded runtime still receives application-stop and configuration
notifications while its package is absent. In particular, suppressing stop
notifications would retain pointers to IIS-owned contexts after their destruction.
Lazy application preparation must synchronize with these notifications.

The current lifecycle matrix is
[`Run-IisRumLifecycle.ps1`](../../test/lifecycle/Run-IisRumLifecycle.ps1).
Run it against the newly built archive before making a host claim.
