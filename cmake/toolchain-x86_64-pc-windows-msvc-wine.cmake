# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

# Wine cross-compilation toolchain — runs MSVC cl.exe / link.exe via Wine on
# Linux.  Contains workarounds for Wine limitations while producing correct
# Windows binaries with embedded manifests.

set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_VERSION 10.0)
set(CMAKE_SYSTEM_PROCESSOR x86_64)
set(CMAKE_C_COMPILER cl.exe)
set(CMAKE_CXX_COMPILER cl.exe)
set(CMAKE_RC_COMPILER rc.exe)
set(CMAKE_LINKER link.exe)
set(CMAKE_AR lib.exe)

# --- Wine workaround: mspdbsrv.exe (PDB manager) ---
# mspdbsrv.exe cannot run under Wine, so /Zi (which needs PDB service) is
# replaced with /Z7 (debug info embedded in .obj files).
# CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY skips link tests that would
# also fail under Wine.
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)
set(CMAKE_C_FLAGS_DEBUG "/Z7 /Ob0 /Od /RTC1" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_DEBUG "/Z7 /Ob0 /Od /RTC1" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_RELWITHDEBINFO "/Z7 /O2 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_RELWITHDEBINFO "/Z7 /O2 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)

# --- Wine workaround: mt.exe (manifest tool) ---
# mt.exe crashes under Wine.  Instead:
#   1. /MANIFEST:EMBED tells link.exe to embed the manifest as a Win32 resource
#      directly during linking — no separate mt.exe pass needed.
#   2. CMAKE_MT is set to /usr/bin/true (a no-op) so that CMake's vs_link_dll /
#      vs_link_exe wrappers don't fail when they unconditionally invoke mt.exe.
set(CMAKE_MT /usr/bin/true)
set(CMAKE_EXE_LINKER_FLAGS_INIT "/MANIFEST:EMBED")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "/MANIFEST:EMBED")

# --- Wine workaround: pthreads false detection ---
# FindThreads detects the Linux host's pthreads (because TRY_COMPILE_TARGET_TYPE
# is STATIC_LIBRARY and the compile-only test succeeds against host headers).
# GoogleTest then fails looking for pthread.h in the Windows SDK.
set(THREADS_PREFER_PTHREAD_FLAG OFF)
set(gtest_disable_pthreads ON CACHE BOOL "No pthreads on Windows cross-compile" FORCE)
