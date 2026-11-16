# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.24)

include(cmake/version.cmake)

project(iis-injector LANGUAGES CXX VERSION ${IIS_INJECTOR_VERSION})

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(BUILD_STATIC_LIBS ON)
set(BUILD_SHARED_LIBS OFF)

option(IIS_BUILD_TESTS "Build unit tests" OFF)
option(IIS_INJECTOR_STATIC_CRT "Build dd-trace-cpp with static CRT with MSVC" ON)
option(IIS_INJECTOR_DEBUG_BREAK "Set breakpoint at startup" OFF)

macro(get_WIN32_WINNT version)
  if(CMAKE_SYSTEM_VERSION)
    set(ver ${CMAKE_SYSTEM_VERSION})
    string(REGEX MATCH "^([0-9]+).([0-9])" ver ${ver})
    string(REGEX MATCH "^([0-9]+)" verMajor ${ver})

    # Check for Windows 10, b/c we'll need to convert to hex 'A'.
    if("${verMajor}" MATCHES "10")
      set(verMajor "A")
      string(REGEX REPLACE "^([0-9]+)" ${verMajor} ver ${ver})
    endif()

    string(REPLACE "." "" ver ${ver})
    # Prepend each digit with a zero.
    string(REGEX REPLACE "([0-9A-Z])" "0\\1" ver ${ver})
    set(${version} "0x${ver}")
  endif()
endmacro()

get_WIN32_WINNT(win_ver)

if (IIS_INJECTOR_STATIC_CRT)
  set(DD_TRACE_STATIC_CRT ON)
  set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
  set(CMAKE_C_FLAGS_RELEASE "${CMAKE_C_FLAGS_RELEASE} /MT")
  set(CMAKE_C_FLAGS_DEBUG "${CMAKE_C_FLAGS_DEBUG} /MTd")
endif ()

# corrosion
include(FetchContent)

FetchContent_Declare(
    Corrosion
    GIT_REPOSITORY https://github.com/corrosion-rs/corrosion.git
    GIT_TAG v0.5 # Optionally specify a commit hash, version tag or branch here
)
# Set any global configuration variables such as `Rust_TOOLCHAIN` before this line!
FetchContent_MakeAvailable(Corrosion)

## dd-trace-cpp
set(DD_TRACE_STATIC_CRT ON)
FetchContent_Declare(
  dd-trace-cpp
  GIT_REPOSITORY https://github.com/DataDog/dd-trace-cpp
  GIT_TAG        5d04dfd395b95b69f1c83f0d23324124e745dfb0 # v2.1.1
  GIT_SHALLOW    OFF
  GIT_PROGRESS   ON
)

FetchContent_MakeAvailable(dd-trace-cpp)

corrosion_import_crate(
  MANIFEST_PATH ${CMAKE_SOURCE_DIR}/../Cargo.toml
  CRATE_TYPES staticlib
)

corrosion_experimental_cbindgen(
  TARGET inject_browser_sdk_ffi
  HEADER_NAME injectbrowsersdk.h
  FLAGS --config ${CMAKE_SOURCE_DIR}/cbindgen.toml
)

# RUM_SDK_INJECTOR_VERSION is used by version.cpp.in template
set(RUM_SDK_INJECTOR_VERSION "${IIS_INJECTOR_VERSION}")

configure_file(
  ${CMAKE_CURRENT_SOURCE_DIR}/src/version.cpp.in
  ${CMAKE_BINARY_DIR}/generated/version.cpp
)

add_library(iis_injector-objects OBJECT)
add_library(iis_injector::obj ALIAS iis_injector-objects)

target_sources(iis_injector-objects
  PRIVATE
    src/config_reader.cpp
    src/entrypoint.cpp
    src/global_module.cpp
    src/http_module.cpp
    src/module_context.cpp
    src/telemetry.cpp
    src/ruminjector.rc
    ${CMAKE_BINARY_DIR}/generated/version.cpp
)

find_program(MC mc)

if (MC STREQUAL "MC-NOTFOUND")
  message(WARNING "mc not found. Can't compile the message text")
else ()
  add_custom_command(
    OUTPUT ${CMAKE_BINARY_DIR}/generated/event_provider.rc
    COMMAND mc -h ${CMAKE_CURRENT_SOURCE_DIR}/src -r ${CMAKE_BINARY_DIR}/generated ${CMAKE_CURRENT_SOURCE_DIR}/src/event_provider.mc
    COMMENT "Compiling message text"
  )

  target_sources(iis_injector-objects
    PRIVATE
      ${CMAKE_BINARY_DIR}/generated/event_provider.rc
  )
endif()

target_include_directories(iis_injector-objects
  PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_compile_options(iis_injector-objects
  PRIVATE
    /W4
    /DUNICODE
    /D_CRT_SECURE_NO_WARNINGS
    /D_WIN32_WINNT=${win_ver}
    /DMAJ_VER=${IIS_INJECTOR_VERSION_MAJ}
    /DMIN_VER=${IIS_INJECTOR_VERSION_MIN}
    /DBUILD_VER=${IIS_INJECTOR_VERSION_BUILD}
    /DVERSION_PRE="${IIS_INJECTOR_VERSION_PRE}"
)

if (IIS_INJECTOR_DEBUG_BREAK)
  target_compile_options(iis_injector-objects
    PRIVATE
      /DDD_INJECTOR_BREAK=1
  )
endif ()

option(IIS_ENABLE_ASAN "Enable Address Sanitizer" OFF)

if (CMAKE_BUILD_TYPE STREQUAL "Debug")
  target_compile_options(iis_injector-objects
    PRIVATE
      # Embedded debug information in binaries (no pdb)
      /Z7
  )
endif ()

target_link_libraries(iis_injector-objects
  PUBLIC
  inject_browser_sdk_ffi dd-trace-cpp::static
  ws2_32.lib
  oleaut32.lib)

## shared lib
add_library(iis_injector SHARED)
target_link_libraries(iis_injector iis_injector-objects)

## Tests
FetchContent_Declare(
  googletest
  URL https://github.com/google/googletest/archive/refs/tags/v1.18.0.zip
  URL_HASH SHA256=63b9c77751a5b8f492486005f67533fdc58682b67476fcb91650be5958d5195a
)

set(gtest_force_shared_crt OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(googletest)

if (IIS_BUILD_TESTS)
  enable_testing()
endif ()

add_subdirectory(bootstrap)

if (IIS_BUILD_TESTS)
  add_subdirectory(test/unittests)
endif ()
