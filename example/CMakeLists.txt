# Example upstream consumer of inject-browser-sdk
#
# Demonstrates how add_subdirectory() exposes INJECT_BROWSER_SDK_VERSION
# and INJECT_BROWSER_SDK_GIT_SHA_SHORT to the parent scope, similar to how
# nginx-datadog consumes it.
#
# Build:
#   cmake -B build example
#   cmake --build build

cmake_minimum_required(VERSION 3.22)
project(ExampleConsumer LANGUAGES C CXX)

# --- Bring in inject-browser-sdk via add_subdirectory ---
# This sets INJECT_BROWSER_SDK_VERSION and INJECT_BROWSER_SDK_GIT_SHA_SHORT in our scope.
add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/.. ${CMAKE_CURRENT_BINARY_DIR}/inject-browser-sdk)

message(STATUS "INJECT_BROWSER_SDK_PKGID          = ${INJECT_BROWSER_SDK_PKGID}")
message(STATUS "INJECT_BROWSER_SDK_GIT_SHA_SHORT  = ${INJECT_BROWSER_SDK_GIT_SHA_SHORT}")
message(STATUS "INJECT_BROWSER_SDK_VERSION         = ${INJECT_BROWSER_SDK_VERSION}")

# --- Use the variables (mirrors nginx-datadog pattern) ---
set(RUM_SDK_INJECTOR_VERSION "${INJECT_BROWSER_SDK_VERSION}")

# Generate a version header from the template
configure_file(
  ${CMAKE_CURRENT_SOURCE_DIR}/version.h.in
  ${CMAKE_CURRENT_BINARY_DIR}/version.h
  @ONLY
)

# Minimal executable that prints the version
add_executable(example_consumer main.cpp)
target_include_directories(example_consumer PRIVATE ${CMAKE_CURRENT_BINARY_DIR})
target_link_libraries(example_consumer PRIVATE inject_browser_sdk_ffi)
