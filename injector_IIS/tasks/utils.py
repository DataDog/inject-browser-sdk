# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""
Miscellaneous functions, no tasks here
"""
from __future__ import print_function

import os
import re


# Path to the workspace Cargo.toml relative to the injector_IIS directory
_CARGO_TOML_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.realpath(__file__))), "..", "Cargo.toml"
)


def _parse_cargo_version(cargo_toml_path=_CARGO_TOML_PATH):
    """Parse the workspace version from Cargo.toml.

    Returns (numeric_version, prerelease) where:
      - numeric_version is "X.Y.Z"
      - prerelease is the suffix after the hyphen (e.g., "dev") or "" if none
    """
    with open(cargo_toml_path) as f:
        for line in f:
            m = re.match(r'^version\s*=\s*"(\d+\.\d+\.\d+)(?:-(.+))?"', line)
            if m:
                return m.group(1), m.group(2) or ""

    raise Exception(f"Could not find version in {cargo_toml_path}")


def get_version_numeric_only(ctx=None, env=None, major_version=None):
    """Return the numeric-only version (e.g., "1.2.1") from Cargo.toml."""
    version, _ = _parse_cargo_version()
    return version


def get_version(ctx=None, include_git=False, url_safe=False, git_sha_length=7,
                prefix=None, env=None, major_version=None):
    """Return the full version string (e.g., "1.2.1-dev") from Cargo.toml."""
    numeric, pre = _parse_cargo_version()
    if pre:
        return f"{numeric}-{pre}"
    return numeric
