# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

cmake_minimum_required(VERSION 3.22)

# Only declare a project when built standalone (not via FetchContent)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
  project(InjectBrowserSDK LANGUAGES C CXX)

  # Emit a minimal C++ compilation unit so that build-tracing tools
  # (e.g. CodeQL autobuild) see at least one C++ compiler invocation.
  file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/_empty.cpp" "")
  add_library(_inject_browser_sdk_probe STATIC
              "${CMAKE_CURRENT_BINARY_DIR}/_empty.cpp")
endif()

# ---------------------------------------------------------------------------
# Expose this library's version to consumers
# ---------------------------------------------------------------------------
execute_process(
  COMMAND cargo pkgid --package inject-browser-sdk --manifest-path "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
  OUTPUT_VARIABLE _INJECT_BROWSER_SDK_CARGO_PKGID
  OUTPUT_STRIP_TRAILING_WHITESPACE
)
string(REGEX REPLACE ".*[#@](.*)" "\\1" INJECT_BROWSER_SDK_PKGID "${_INJECT_BROWSER_SDK_CARGO_PKGID}")
set(INJECT_BROWSER_SDK_PKGID "${INJECT_BROWSER_SDK_PKGID}" PARENT_SCOPE)

# ---------------------------------------------------------------------------
# Expose this library's git SHA to consumers
# ---------------------------------------------------------------------------
execute_process(
  COMMAND git rev-parse HEAD
  WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
  OUTPUT_VARIABLE INJECT_BROWSER_SDK_GIT_SHA
  OUTPUT_STRIP_TRAILING_WHITESPACE
)
set(INJECT_BROWSER_SDK_GIT_SHA "${INJECT_BROWSER_SDK_GIT_SHA}" PARENT_SCOPE)
execute_process(
  COMMAND git rev-parse --short HEAD
  WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
  OUTPUT_VARIABLE INJECT_BROWSER_SDK_GIT_SHA_SHORT
  OUTPUT_STRIP_TRAILING_WHITESPACE
)
set(INJECT_BROWSER_SDK_GIT_SHA_SHORT "${INJECT_BROWSER_SDK_GIT_SHA_SHORT}" PARENT_SCOPE)

# ---------------------------------------------------------------------------
# Combined version: semver + short SHA (e.g. "0.1.0+0b2cc63")
# ---------------------------------------------------------------------------
set(INJECT_BROWSER_SDK_VERSION "${INJECT_BROWSER_SDK_PKGID}+${INJECT_BROWSER_SDK_GIT_SHA_SHORT}")
set(INJECT_BROWSER_SDK_VERSION "${INJECT_BROWSER_SDK_VERSION}" PARENT_SCOPE)

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
