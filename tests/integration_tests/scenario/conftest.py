# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.
import pytest

import threading
from flask import Flask, abort


class FlaskHTTPServer:
    def __init__(self, app, host, port) -> None:
        self._thread = None
        self._host = host
        self._port = port
        self._app = app
        self._routes = {}

    def _catch_all(self, path: str):
        view_func = self._routes.get(f"/{path}")
        if view_func:
            return view_func()
        else:
            abort(404, description=f"No view found for path: /{path}")

    def internal_run(self) -> None:
        self._app.add_url_rule(
            "/<path:path>", view_func=self._catch_all, methods=["GET"]
        )
        self._app.run(host=self._host, port=self._port)

    def run(self) -> None:
        self._thread = threading.Thread(target=self.internal_run)
        self._thread.daemon = True
        self._thread.start()

    def stop(self) -> None:
        pass

    def add_route(self, path: str, handler) -> None:
        self._routes[path] = handler


def make_temporary_http_server(host: str, port: int) -> FlaskHTTPServer:
    app = Flask(__name__)
    return FlaskHTTPServer(app, host, port)


def pytest_sessionstart(session: pytest.Session) -> None:
    """
    Called after the Session object has been created and
    before performing collection and entering the run test loop.
    """
    session.config.upstream_server = make_temporary_http_server(
        host="localhost", port=8090
    )
    session.config.upstream_server.run()


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    """
    Called after whole test run finished, right before
    returning the exit status to the system.
    """
    try:
        session.config.upstream_server.stop()
    except:
        pass


@pytest.fixture
def upstream_server(request):
    return request.config.upstream_server
