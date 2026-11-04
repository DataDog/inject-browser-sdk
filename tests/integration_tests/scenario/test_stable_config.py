# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""Stable-configuration scenario: RUM configured centrally, not per site.

Requires the environment produced by
`setup-integration-test-env.ps1 -StableConfig` followed by
`run-integration-tests.ps1 -StableConfig`. Setup adds a third site with no IIS
RUM configuration; the runner temporarily installs application_monitoring.yaml
and restores the original after the session. Skipped otherwise. The host must
have no fleet-managed stable config, which takes precedence over the fixture.

Ports:
  8080  TestRumEnabled       -- configured through IIS (applicationId "foo",
                                site datadoghq.eu)
  8081  TestRumDisabled      -- explicit enabled="false"
  8082  TestRumStableConfig  -- no IIS section; stable config only
"""

import os

import pytest
import requests
from aiohttp import web

SIMPLE_HTML = """<html>
        <head>
            <title>Upstream</title>
        </head>
        <body>
            <p> Hello, Dog! </p>
        </body>
        </html>
"""

# Values written temporarily by run-integration-tests.ps1 -StableConfig.
STABLE_APPLICATION_ID = "stable-config-app-id"
STABLE_CLIENT_TOKEN = "stable-config-client-token"

# Values set per-site through IIS on port 8080.
IIS_APPLICATION_ID = "foo"

pytestmark = pytest.mark.skipif(
    os.environ.get("DD_RUM_STABLE_CONFIG_TEST", "") != "1",
    reason="requires setup and run-integration-tests.ps1 -StableConfig",
)


def _fetch(upstream_server, port, route):
    """Proxy a request through the given site and return (headers, body)."""

    async def upstream_handler(request):
        request.app["ctx"]["last_request"] = request
        return web.Response(body=SIMPLE_HTML, content_type="text/html")

    upstream_server.add_route(route, upstream_handler)
    response = requests.get(f"http://localhost:{port}{route}")
    assert response.status_code == 200
    return response.headers, response.content.decode()


def test_injects_on_a_site_with_no_iis_configuration(upstream_server):
    """The whole point: a site nobody configured still gets the SDK."""
    headers, body = _fetch(upstream_server, 8082, "/stable_only")

    assert headers["x-datadog-rum-injected"] == "1"
    assert "datadog-rum.js" in body
    assert STABLE_APPLICATION_ID in body
    assert STABLE_CLIENT_TOKEN in body


def test_major_version_comes_from_stable_config(upstream_server):
    """DD_RUM_MAJOR_VERSION: 6 must reach the CDN URL."""
    _, body = _fetch(upstream_server, 8082, "/stable_only_version")

    assert "/v6/" in body, body


def test_iis_section_overrides_stable_config_per_key(upstream_server):
    """A site configured through IIS keeps its own values, unchanged from before
    stable configuration existed."""
    headers, body = _fetch(upstream_server, 8080, "/iis_override")

    assert headers["x-datadog-rum-injected"] == "1"
    assert IIS_APPLICATION_ID in body
    assert STABLE_APPLICATION_ID not in body, "the IIS value must win"
    # datadoghq.eu maps to the eu1 CDN region; datadoghq.com would be us1.
    assert "/eu1/" in body, body


def test_explicit_iis_opt_out_beats_stable_config(upstream_server):
    """enabled="false" is the per-site escape hatch and must outrank the file."""
    headers, body = _fetch(upstream_server, 8081, "/opted_out")

    assert "x-datadog-rum-injected" not in headers
    assert "datadog-rum.js" not in body
