#!/usr/bin/env bash
# Integration test: compile the IIS module, install it in IIS Express under
# Wine, serve an HTML page, and verify the RUM snippet was injected.
#
# Usage:
#   bash injector_IIS/test/integration/run_iisexpress_test.sh
#
# Expects to run inside the Windows cross-compilation Docker image (or any
# environment where Wine, IIS Express at /opt/iis_express, and the built
# iis_injector.dll are available).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

DLL_PATH="$REPO_ROOT/injector_IIS/build/iis_injector.dll"
SCHEMA_SRC="$REPO_ROOT/injector_IIS/resources/RUM_schema.xml"
IIS_EXPRESS_DIR="/opt/iis_express"
IIS_EXPRESS_EXE="$IIS_EXPRESS_DIR/iisexpress.exe"
PORT=8099
TEST_OUTPUT="$SCRIPT_DIR/test_output"

FAILURES=0
IIS_PID=""
WORK_DIR=""

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

log()  { echo "--- $*"; }
pass() { echo "  PASS: $1"; }

fail() {
    echo "  FAIL: $1"
    FAILURES=$((FAILURES + 1))
}

assert_file_exists() {
    if [[ ! -f "$1" ]]; then
        echo "ERROR: required file not found: $1 ($2)"
        exit 2
    fi
}

assert_grep() {
    local pattern="$1" file="$2" label="$3"
    if grep -qi "$pattern" "$file" 2>/dev/null; then
        pass "$label"
    else
        fail "$label"
    fi
}

cleanup() {
    if [[ -n "$IIS_PID" ]] && kill -0 "$IIS_PID" 2>/dev/null; then
        kill "$IIS_PID" 2>/dev/null || true
        wait "$IIS_PID" 2>/dev/null || true
    fi
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Phase 0: Validation
# ---------------------------------------------------------------------------

log "Phase 0: Validating prerequisites"

assert_file_exists "$DLL_PATH"        "build artifact iis_injector.dll"
assert_file_exists "$SCHEMA_SRC"      "RUM configuration schema"
assert_file_exists "$IIS_EXPRESS_EXE" "IIS Express executable"

if ! command -v curl &>/dev/null; then
    echo "ERROR: curl is not installed"
    exit 2
fi

if ! command -v wine &>/dev/null; then
    echo "ERROR: wine is not installed"
    exit 2
fi

# IIS Express requires mscoree.dll (.NET CLR shim) in its own directory.
# Wine Mono installs it in C:\windows\system32 during wineboot, so copy it
# into the IIS Express directory where the loader will find it.
MSCOREE_SRC="${WINEPREFIX:-/tmp/wine}/drive_c/windows/system32/mscoree.dll"
if [[ -f "$MSCOREE_SRC" ]]; then
    cp "$MSCOREE_SRC" "$IIS_EXPRESS_DIR/mscoree.dll"
    log "Copied mscoree.dll into IIS Express directory"
else
    echo "WARNING: mscoree.dll not found at $MSCOREE_SRC — IIS Express may fail to start"
fi

# ---------------------------------------------------------------------------
# Phase 1: Setup
# ---------------------------------------------------------------------------

log "Phase 1: Setting up test environment"

WORK_DIR="$(mktemp -d)"
SITE_ROOT="$WORK_DIR/site"
mkdir -p "$SITE_ROOT"
mkdir -p "$TEST_OUTPUT"

# Copy test page and web.config into the site root.
cp "$SCRIPT_DIR/index.html"  "$SITE_ROOT/index.html"
cp "$SCRIPT_DIR/web.config"  "$SITE_ROOT/web.config"

# Install the RUM schema where IIS Express can find it.
mkdir -p "$IIS_EXPRESS_DIR/config/schema"
cp "$SCHEMA_SRC" "$IIS_EXPRESS_DIR/config/schema/RUM_schema.xml"

# Translate Linux paths to Windows paths for IIS Express.
WIN_DLL_PATH="$(winepath -w "$DLL_PATH" 2>/dev/null)"
WIN_SITE_ROOT="$(winepath -w "$SITE_ROOT" 2>/dev/null)"
WIN_CONFIG="$(winepath -w "$WORK_DIR/applicationHost.config" 2>/dev/null)"

log "  DLL (Windows path):  $WIN_DLL_PATH"
log "  Site root (Windows): $WIN_SITE_ROOT"

# Generate applicationHost.config from template.
sed -e "s|@@DLL_PATH@@|$WIN_DLL_PATH|g" \
    -e "s|@@SITE_ROOT@@|$WIN_SITE_ROOT|g" \
    -e "s|@@PORT@@|$PORT|g" \
    "$SCRIPT_DIR/applicationHost.config.template" > "$WORK_DIR/applicationHost.config"

# Also set IIS_BIN in case IIS Express does not set it internally under Wine.
export IIS_BIN
IIS_BIN="$(winepath -w "$IIS_EXPRESS_DIR" 2>/dev/null)"

# ---------------------------------------------------------------------------
# Phase 2: Start IIS Express
# ---------------------------------------------------------------------------

log "Phase 2: Starting IIS Express (port $PORT)"

wine "$IIS_EXPRESS_EXE" \
    "/config:$WIN_CONFIG" \
    /site:TestSite \
    /trace:error \
    > "$TEST_OUTPUT/iisexpress_stdout.log" 2> "$TEST_OUTPUT/wine.log" &
IIS_PID=$!

# ---------------------------------------------------------------------------
# Phase 3: Wait for readiness
# ---------------------------------------------------------------------------

log "Phase 3: Waiting for IIS Express to become ready"

TIMEOUT=30
READY=0
for i in $(seq 1 "$TIMEOUT"); do
    if ! kill -0 "$IIS_PID" 2>/dev/null; then
        echo "FAIL: IIS Express process died during startup"
        echo "--- Last 50 lines of Wine log ---"
        tail -50 "$TEST_OUTPUT/wine.log" 2>/dev/null || true
        echo "--- IIS Express stdout ---"
        cat "$TEST_OUTPUT/iisexpress_stdout.log" 2>/dev/null || true
        exit 1
    fi

    HTTP_CODE="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$PORT/" 2>/dev/null || true)"
    if [[ "$HTTP_CODE" =~ ^[2345] ]]; then
        log "IIS Express ready after ${i}s (HTTP $HTTP_CODE)"
        READY=1
        break
    fi
    sleep 1
done

if [[ "$READY" -eq 0 ]]; then
    echo "FAIL: IIS Express not ready after ${TIMEOUT}s"
    echo "--- Last 50 lines of Wine log ---"
    tail -50 "$TEST_OUTPUT/wine.log" 2>/dev/null || true
    exit 1
fi

# ---------------------------------------------------------------------------
# Phase 4: Test assertions
# ---------------------------------------------------------------------------

log "Phase 4: Fetching page and checking injection"

curl -s -D "$TEST_OUTPUT/headers.txt" "http://localhost:$PORT/index.html" \
    > "$TEST_OUTPUT/response.html" 2>/dev/null

echo "--- Response headers ---"
cat "$TEST_OUTPUT/headers.txt"
echo ""
echo "--- Response body ---"
cat "$TEST_OUTPUT/response.html"
echo ""

# Check HTTP status
assert_grep "200" "$TEST_OUTPUT/headers.txt" "HTTP status 200"

# Check the RUM injection occurred
assert_grep "DD_RUM"                "$TEST_OUTPUT/response.html"  "DD_RUM script present in body"
assert_grep "test-app-id"           "$TEST_OUTPUT/response.html"  "applicationId in snippet"
assert_grep "test-client-token"     "$TEST_OUTPUT/response.html"  "clientToken in snippet"
assert_grep "test-service"          "$TEST_OUTPUT/response.html"  "service name in snippet"
assert_grep "x-datadog-rum-injected" "$TEST_OUTPUT/headers.txt"   "x-datadog-rum-injected response header"

# Sanity: the original page content is still there
assert_grep "Integration Test"      "$TEST_OUTPUT/response.html"  "original page title preserved"

# ---------------------------------------------------------------------------
# Phase 5: Report
# ---------------------------------------------------------------------------

echo ""
if [[ "$FAILURES" -eq 0 ]]; then
    echo "===== PASS: IIS Express integration test ====="
    exit 0
else
    echo "===== FAIL: $FAILURES check(s) failed ====="
    exit 1
fi
