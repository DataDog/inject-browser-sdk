#!/bin/sh
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

# Drives the installer through install and uninstall inside a real nginx or httpd
# container, then checks what the unit tests cannot: that the config file comes
# back byte for byte with its mode, owner and symlink identity intact, that the
# installation directory is gone, and that the server still starts and answers.
#
# Set CONFIG_SYMLINK=1 to put the config behind a symlink first, which is how
# distro-managed configs are often laid out.

set -eu

PROXY_KIND="${PROXY_KIND:?PROXY_KIND must be set to 'nginx' or 'httpd'}"
CONFIG_SYMLINK="${CONFIG_SYMLINK:-}"
INSTALLER_DIR="${INSTALLER_DIR:-/installer}"
AGENT_URI="${AGENT_URI:-http://datadog-agent:8126}"
SNAPSHOT_DIR="${SNAPSHOT_DIR:-/tmp/proxy-test}"

HTTPD_CMD=""

log() {
    echo "[proxy-test] $*"
}

fail() {
    echo "[proxy-test] FAILED: $*" >&2
    exit 1
}

# ------------------------------------------------------------------ proxy glue

# The first httpd front end on the PATH, the same list the configurator tries.
find_httpd_command() {
    for candidate in httpd apachectl apache2 apache2ctl; do
        if command -v "$candidate" >/dev/null 2>&1; then
            HTTPD_CMD="$candidate"
            return 0
        fi
    done

    fail "no httpd front end binary found on the PATH"
}

# Asks the proxy itself where its config lives, rather than hardcoding a path per
# base image.
proxy_config_path() {
    if [ "$PROXY_KIND" = nginx ]; then
        nginx -T 2>/dev/null | sed -n 's/^# configuration file \(.*\):$/\1/p' | head -n 1
        return 0
    fi

    info=$("$HTTPD_CMD" -V)
    config=$(echo "$info" | sed -n 's/.*SERVER_CONFIG_FILE="\([^"]*\)".*/\1/p')

    case "$config" in
        /*) echo "$config" ;;
        *)  echo "$(echo "$info" | sed -n 's/.*HTTPD_ROOT="\([^"]*\)".*/\1/p')/$config" ;;
    esac
}

validate_config() {
    if [ "$PROXY_KIND" = nginx ]; then
        nginx -t
    else
        "$HTTPD_CMD" -t
    fi
}

start_proxy() {
    if [ "$PROXY_KIND" = nginx ]; then
        nginx
    else
        "$HTTPD_CMD" -k start
    fi
}

stop_proxy() {
    if [ "$PROXY_KIND" = nginx ]; then
        nginx -s stop || true
    else
        "$HTTPD_CMD" -k stop || true
    fi

    # Give the listening socket time to go away before the next phase starts the
    # server again.
    sleep 1
}

run_install() {
    sh "$INSTALLER_DIR/install-proxy-datadog.sh" \
        --proxyKind "$PROXY_KIND" --verbose --appId 123 --site datadoghq.com \
        --clientToken myToken --sessionSampleRate 100 --sessionReplaySampleRate 50 \
        --agentUri "$AGENT_URI" --skipDownload
}

run_uninstall() {
    sh "$INSTALLER_DIR/install-proxy-datadog.sh" \
        --proxyKind "$PROXY_KIND" --verbose --uninstall --skipDownload
}

# ------------------------------------------------------------------- assertions

# Byte-exact comparison. md5sum rather than cmp, which comes from diffutils and is
# absent from some of the base images under test.
assert_same_bytes() {
    expected_sum=$(md5sum < "$1")
    actual_sum=$(md5sum < "$2")

    if [ "$expected_sum" != "$actual_sum" ]; then
        echo "[proxy-test] --- expected ($1) ---" >&2
        cat "$1" >&2
        echo "[proxy-test] --- actual ($2) ---" >&2
        cat "$2" >&2
        fail "$3"
    fi

    log "$4"
}

assert_config_serves() {
    validate_config || fail "$1: the config does not validate"

    start_proxy

    # Any status code counts: some base images answer '/' with a 403 welcome page.
    # What is being tested is that the server came up and served a request at all.
    code=000
    attempt=0
    while [ "$attempt" -lt 15 ]; do
        # curl exits non-zero when it never got a response, which is the only case
        # the retry is for; on success the status code it printed is meaningful.
        if code=$(curl -s -o /dev/null -w '%{http_code}' http://localhost/ 2>/dev/null); then
            break
        fi

        code=000
        attempt=$((attempt + 1))
        sleep 1
    done

    stop_proxy

    if [ "$code" = "000" ]; then
        fail "$1: the proxy never answered an HTTP request"
    fi

    log "$1: the config validates and the proxy answered with HTTP $code"
}

# Nothing may be left pointing at the installation directory, and no candidate
# config may be left lying around.
assert_uninstalled() {
    [ ! -e "$INSTALL_DIR" ] || fail "$1: the installation directory '$INSTALL_DIR' still exists"

    leftovers=$(find "$CONFIG_DIR" -name '*.datadog' 2>/dev/null || true)
    [ -z "$leftovers" ] || fail "$1: candidate config files were left behind: $leftovers"

    if grep -qF "$INSTALL_DIR" "$REAL_CONFIG"; then
        fail "$1: the config still references '$INSTALL_DIR'"
    fi
}

assert_config_unchanged() {
    assert_same_bytes "$PRISTINE_CONFIG" "$REAL_CONFIG" \
        "$1: the config did not come back byte for byte" \
        "$1: the config is byte for byte the original"

    actual_stat=$(stat -c '%a %u %g' "$REAL_CONFIG")
    if [ "$actual_stat" != "$PRISTINE_STAT" ]; then
        fail "$1: the config's mode/owner changed from '$PRISTINE_STAT' to '$actual_stat'"
    fi

    if [ "$CONFIG_SYMLINK" = 1 ] && [ ! -L "$CONFIG_PATH" ]; then
        fail "$1: '$CONFIG_PATH' is no longer a symlink"
    fi

    log "$1: the config kept its mode/owner '$actual_stat'"
}

# ----------------------------------------------------------------------- phase 0

cd "$INSTALLER_DIR"

if [ "$PROXY_KIND" = httpd ]; then
    find_httpd_command
    INSTALL_DIR=/opt/datadog-httpd
    INSTALL_MARKER="Include /opt/datadog-httpd/datadog.conf"
else
    INSTALL_DIR=/opt/datadog-nginx
    INSTALL_MARKER="ngx_http_datadog_module.so"
fi

CONFIG_PATH=$(proxy_config_path)
[ -n "$CONFIG_PATH" ] || fail "could not determine the $PROXY_KIND config path"
CONFIG_DIR=$(dirname "$CONFIG_PATH")

log "P0: preparing '$CONFIG_PATH'"

REAL_CONFIG="$CONFIG_PATH"
if [ "$CONFIG_SYMLINK" = 1 ]; then
    # The layout this covers: the path the proxy reports is a symlink, and the
    # config the installer must write through is somewhere else entirely.
    REAL_CONFIG="$CONFIG_PATH.real"
    mv "$CONFIG_PATH" "$REAL_CONFIG"
    ln -s "$REAL_CONFIG" "$CONFIG_PATH"
    log "P0: '$CONFIG_PATH' is now a symlink to '$REAL_CONFIG'"
fi

# A restrictive, non-default mode, so a write that recreates the file instead of
# updating it in place is visible as a mode change rather than passing unnoticed.
chmod 0640 "$REAL_CONFIG"

mkdir -p "$SNAPSHOT_DIR"
PRISTINE_CONFIG="$SNAPSHOT_DIR/pristine.conf"
cp "$REAL_CONFIG" "$PRISTINE_CONFIG"
PRISTINE_STAT=$(stat -c '%a %u %g' "$REAL_CONFIG")

log "P0: snapshotted the pristine config, mode/owner '$PRISTINE_STAT'"

# ----------------------------------------------------------------------- phase 1

log "P1: installing"
run_install

grep -q "$INSTALL_MARKER" "$REAL_CONFIG" || fail "P1: the config does not reference the datadog module"
[ -d "$INSTALL_DIR" ] || fail "P1: the installation directory '$INSTALL_DIR' was not created"

assert_config_serves "P1"

# ----------------------------------------------------------------------- phase 2

log "P2: uninstalling"
run_uninstall

assert_uninstalled "P2"
assert_config_unchanged "P2"
assert_config_serves "P2"

# ----------------------------------------------------------------------- phase 3

# A duplicate Include still names a file inside the installation directory, so
# removing only the first occurrence passes 'httpd -t' — datadog.conf is still
# there while the test runs — and leaves Apache unable to start once the
# directory is deleted.
#
# Not reachable for nginx: two load_module lines make 'nginx -T' fail with
# "module is already loaded", so the installer cannot even find the config path.
# nginx multi-occurrence removal is covered by unit tests instead.
if [ "$PROXY_KIND" = httpd ]; then
    log "P3: installing, then duplicating the datadog Include by hand"
    run_install

    printf '%s\n' "$INSTALL_MARKER" >> "$REAL_CONFIG"

    log "P3: uninstalling with two datadog Include lines present"
    run_uninstall

    assert_uninstalled "P3"
    assert_same_bytes "$PRISTINE_CONFIG" "$REAL_CONFIG" \
        "P3: the config did not come back byte for byte" \
        "P3: the config is byte for byte the original, with both Include lines gone"
    assert_config_serves "P3"
fi

# ----------------------------------------------------------------------- phase 4

# The installer writes "Disable APM Tracing. Remove the next line to enable APM
# Tracing." above datadog_tracing. A user who follows that instruction must still
# be able to uninstall, and the comment must not survive as an orphan.
if [ "$PROXY_KIND" = nginx ]; then
    log "P4: installing, then deleting datadog_tracing as our own comment invites"
    run_install

    sed -i '/datadog_tracing off;/d' "$REAL_CONFIG"
    if grep -q 'datadog_tracing' "$REAL_CONFIG"; then
        fail "P4: failed to delete the datadog_tracing directive"
    fi

    log "P4: uninstalling a hand-edited config"
    run_uninstall

    assert_uninstalled "P4"
    assert_same_bytes "$PRISTINE_CONFIG" "$REAL_CONFIG" \
        "P4: the config did not come back byte for byte" \
        "P4: the config is byte for byte the original, with no orphaned comment left"
    assert_config_serves "P4"
fi

log "All phases passed for $PROXY_KIND (CONFIG_SYMLINK='${CONFIG_SYMLINK}')"
