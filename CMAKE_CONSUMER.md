# CMake Consumer Guide

Consume `inject-browser-sdk-ffi` from your CMake project via `FetchContent`.
The CMake wrapper builds the FFI crate in **no_std** mode by default (by passing `NO_DEFAULT_FEATURES` to the Rust build).

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
