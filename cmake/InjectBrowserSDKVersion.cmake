# InjectBrowserSDKVersion.cmake - Parse version from Cargo.toml (single source of truth)
#
# Sets the following variables in the caller's scope:
#   INJECT_BROWSER_SDK_VERSION     - full semver, e.g. "1.2.1-dev"
#   INJECT_BROWSER_SDK_VERSION_MAJ - major, e.g. "1"
#   INJECT_BROWSER_SDK_VERSION_MIN - minor, e.g. "2"
#   INJECT_BROWSER_SDK_VERSION_PATCH - patch, e.g. "1"
#   INJECT_BROWSER_SDK_VERSION_PRE - pre-release suffix, e.g. "dev" (or "")
#   INJECT_BROWSER_SDK_VERSION_NUMERIC - "MAJ.MIN.PATCH" without pre-release

include_guard(GLOBAL)

if(NOT DEFINED _INJECT_BROWSER_SDK_MANIFEST_PATH)
  set(_INJECT_BROWSER_SDK_MANIFEST_PATH "${CMAKE_CURRENT_LIST_DIR}/../Cargo.toml")
endif()

execute_process(
  COMMAND cargo pkgid --package inject-browser-sdk --manifest-path "${_INJECT_BROWSER_SDK_MANIFEST_PATH}"
  OUTPUT_VARIABLE _IBSV_PKGID
  OUTPUT_STRIP_TRAILING_WHITESPACE
)
string(REGEX REPLACE ".*[@#]" "" _IBSV_FULL "${_IBSV_PKGID}")

if(_IBSV_FULL MATCHES "([0-9]+)\\.([0-9]+)\\.([0-9]+)(-(.+))?")
  set(INJECT_BROWSER_SDK_VERSION     "${CMAKE_MATCH_1}.${CMAKE_MATCH_2}.${CMAKE_MATCH_3}${CMAKE_MATCH_4}")
  set(INJECT_BROWSER_SDK_VERSION_MAJ "${CMAKE_MATCH_1}")
  set(INJECT_BROWSER_SDK_VERSION_MIN "${CMAKE_MATCH_2}")
  set(INJECT_BROWSER_SDK_VERSION_PATCH "${CMAKE_MATCH_3}")
  set(INJECT_BROWSER_SDK_VERSION_PRE "${CMAKE_MATCH_5}")
  set(INJECT_BROWSER_SDK_VERSION_NUMERIC "${CMAKE_MATCH_1}.${CMAKE_MATCH_2}.${CMAKE_MATCH_3}")
else()
  message(FATAL_ERROR "Could not parse version from cargo pkgid: '${_IBSV_FULL}'")
endif()

unset(_IBSV_PKGID)
unset(_IBSV_FULL)
