# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

cmake_minimum_required(VERSION 3.22)

# Only declare a project when built standalone (not via FetchContent)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
  project(InjectBrowserSDK LANGUAGES C CXX)
endif()

# ---------------------------------------------------------------------------
# Rust toolchain check
# ---------------------------------------------------------------------------
find_program(_INJECT_SDK_CARGO cargo)
if(NOT _INJECT_SDK_CARGO)
  if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    message(WARNING "inject-browser-sdk: cargo not found, skipping (install Rust: https://rustup.rs)")
    return()
  else()
    message(FATAL_ERROR "inject-browser-sdk: cargo not found. Install Rust: https://rustup.rs")
  endif()
endif()
unset(_INJECT_SDK_CARGO)

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
set(INJECT_BROWSER_SDK_CORROSION_VERSION "v0.5"
    CACHE STRING "Corrosion git tag to fetch (ignored if consumer already provides Corrosion)")

set(INJECT_BROWSER_SDK_CRATE_TYPES "staticlib"
    CACHE STRING "Crate types passed to corrosion_import_crate (e.g. staticlib, cdylib)")

option(INJECT_BROWSER_SDK_CBINDGEN
       "Run cbindgen to generate the C/C++ header" ON)

option(INJECT_BROWSER_SDK_NO_DEFAULT_FEATURES
       "Pass NO_DEFAULT_FEATURES to corrosion_import_crate (builds in no_std mode)" ON)

# ---------------------------------------------------------------------------
# Corrosion — only fetch if the consumer hasn't loaded it already
# ---------------------------------------------------------------------------
include(FetchContent)

if(NOT COMMAND corrosion_import_crate)
  FetchContent_Declare(
    Corrosion
    GIT_REPOSITORY https://github.com/corrosion-rs/corrosion.git
    GIT_TAG        ${INJECT_BROWSER_SDK_CORROSION_VERSION}
  )
  FetchContent_MakeAvailable(Corrosion)
endif()

# ---------------------------------------------------------------------------
# Import only the FFI crate from the workspace
# ---------------------------------------------------------------------------
set(_inject_sdk_import_args
  MANIFEST_PATH ${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml
  CRATES        inject-browser-sdk-ffi
  CRATE_TYPES   ${INJECT_BROWSER_SDK_CRATE_TYPES}
)

if(INJECT_BROWSER_SDK_NO_DEFAULT_FEATURES)
  list(APPEND _inject_sdk_import_args NO_DEFAULT_FEATURES)
endif()

corrosion_import_crate(${_inject_sdk_import_args})
unset(_inject_sdk_import_args)

# ---------------------------------------------------------------------------
# cbindgen — generate C/C++ header
# ---------------------------------------------------------------------------
if(INJECT_BROWSER_SDK_CBINDGEN)
  corrosion_experimental_cbindgen(
    TARGET      inject_browser_sdk_ffi
    HEADER_NAME injectbrowsersdk.h
    FLAGS       --config ${CMAKE_CURRENT_SOURCE_DIR}/cbindgen.toml
  )
endif()

# ---------------------------------------------------------------------------
# Alias target so consumers can use InjectBrowserSDK::ffi
# ---------------------------------------------------------------------------
if(NOT TARGET InjectBrowserSDK::ffi)
  add_library(InjectBrowserSDK::ffi ALIAS inject_browser_sdk_ffi)
endif()
