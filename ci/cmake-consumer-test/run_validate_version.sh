#!/usr/bin/env bash
# Run the CMake version-variable validation locally.
#
# Uses FETCHCONTENT_SOURCE_DIR to point at the repo root so no
# symlink / copy of the SDK is needed.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUILD_DIR="${BUILD_DIR:-/tmp/cmake-consumer-version-test}"

rm -rf "$BUILD_DIR"

cmake -B "$BUILD_DIR" \
  -DFETCHCONTENT_SOURCE_DIR_INJECTBROWSERSDK="$REPO_ROOT" \
  "$REPO_ROOT/ci/cmake-consumer-test"

cmake --build "$BUILD_DIR" --target validate_version

ctest --test-dir "$BUILD_DIR" -R validate_version --output-on-failure
