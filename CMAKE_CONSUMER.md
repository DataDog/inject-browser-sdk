# CMake Consumer Guide

Consume `inject-browser-sdk-ffi` from your CMake project via `FetchContent`.
The FFI crate is built in **no_std** mode by default.

**Requires:** CMake 3.22+, Rust toolchain, `cbindgen`

## Usage

Copy-paste into your `CMakeLists.txt`:

```cmake
include(FetchContent)

FetchContent_Declare(
  InjectBrowserSDK
  GIT_REPOSITORY https://github.com/DataDog/inject-browser-sdk.git
  GIT_TAG        main
)
FetchContent_MakeAvailable(InjectBrowserSDK)

target_link_libraries(my_target PRIVATE inject_browser_sdk_ffi)
```

You get `inject_browser_sdk_ffi` (static lib), `InjectBrowserSDK::ffi` (alias), and `injectbrowsersdk.h` (auto-generated header).

If your project already loads Corrosion, just make sure it's loaded **before** the `FetchContent_MakeAvailable(InjectBrowserSDK)` call — the wrapper will reuse it.

## Options

Set before `FetchContent_MakeAvailable`:

| Variable | Default | Description |
|---|---|---|
| `INJECT_BROWSER_SDK_CORROSION_VERSION` | `v0.5` | Corrosion git tag (skipped if consumer provides Corrosion) |
| `INJECT_BROWSER_SDK_CRATE_TYPES` | `staticlib` | Crate type(s) for `corrosion_import_crate` |
| `INJECT_BROWSER_SDK_CBINDGEN` | `ON` | Generate `injectbrowsersdk.h` |
| `INJECT_BROWSER_SDK_NO_DEFAULT_FEATURES` | `ON` | no_std mode. Set `OFF` for std. |

## Breaking changes

### `Snippet` grew three fields

`Snippet` gained `rum_enabled`, `application_id`, and `remote_configuration_id`,
appended after the existing members. Code that reads `error_code`,
`error_message`, or `length` through the pointer returned by
`snippet_create_from_json` is unaffected and needs no change.

It **is** an ABI break for code that allocates a `Snippet` by value, embeds one
in another struct, or asserts on `sizeof(Snippet)`. Recompile against the
regenerated `injectbrowsersdk.h`; never mix a new `libinjectbrowsersdk` with an
old header.

`snippet_create_from_json` now populates `application_id` and
`remote_configuration_id` from the parsed configuration (previously always
`NULL`), so hosts can derive telemetry tags from the snippet. `rum_enabled` stays
`RUM_ENABLED_UNSET` for JSON-built snippets: that JSON shape has no `enabled`
key. Only the stable-configuration constructors can report it.

### Default `majorVersion` is now 6

`Configuration::from_lookup` -- the stable-config path, reached through
`stable_config_read`/`snippet_create_from_stable_config` -- defaults
`major_version` to `6` instead of `5` when no `DD_RUM_MAJOR_VERSION` key is
present. It matches the IIS injector's `RUM_schema.xml`, and the agent installer
writes no such key, so the old default silently gave fleet-configured hosts a
different major version than IIS-configured ones.

`snippet_create_from_json` is unaffected: `majorVersion` is a required field of
that JSON shape, so the default was never reachable from it. Set
`DD_RUM_MAJOR_VERSION` explicitly to pin the old behaviour.
