cmake_minimum_required(VERSION 3.22)
project(consumer_test LANGUAGES C CXX)

include(FetchContent)

# CI: symlinks or copies the SDK into ./sdk
# Local: override with -DFETCHCONTENT_SOURCE_DIR_INJECTBROWSERSDK=<path>
FetchContent_Declare(
  InjectBrowserSDK
  SOURCE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/sdk
)
FetchContent_MakeAvailable(InjectBrowserSDK)

# ---------------------------------------------------------------------------
# Existing FFI smoke-test
# ---------------------------------------------------------------------------
add_executable(consumer_test main.cpp)
target_link_libraries(consumer_test PRIVATE inject_browser_sdk_ffi)

option(ENABLE_ASAN "Build with AddressSanitizer" OFF)
if(ENABLE_ASAN)
  target_compile_options(consumer_test PRIVATE -fsanitize=address -fno-omit-frame-pointer)
  target_link_options(consumer_test PRIVATE -fsanitize=address)
endif()

# ---------------------------------------------------------------------------
# Version variable validation (C-only, no Rust toolchain needed at test-time)
# ---------------------------------------------------------------------------
message(STATUS "INJECT_BROWSER_SDK_PKGID          = ${INJECT_BROWSER_SDK_PKGID}")
message(STATUS "INJECT_BROWSER_SDK_GIT_SHA        = ${INJECT_BROWSER_SDK_GIT_SHA}")
message(STATUS "INJECT_BROWSER_SDK_GIT_SHA_SHORT  = ${INJECT_BROWSER_SDK_GIT_SHA_SHORT}")
message(STATUS "INJECT_BROWSER_SDK_VERSION        = ${INJECT_BROWSER_SDK_VERSION}")

configure_file(
  ${CMAKE_CURRENT_SOURCE_DIR}/version_config.h.in
  ${CMAKE_CURRENT_BINARY_DIR}/version_config.h
  @ONLY
)

add_executable(validate_version validate_version.c)
target_include_directories(validate_version PRIVATE ${CMAKE_CURRENT_BINARY_DIR})

enable_testing()
add_test(NAME validate_version COMMAND validate_version)
