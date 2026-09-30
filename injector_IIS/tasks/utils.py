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


# Path to the IIS injector VERSION file
_VERSION_FILE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.realpath(__file__))), "VERSION"
)


def parse_version(version_file=_VERSION_FILE):
    """Parse the IIS injector version from its VERSION file.

    The VERSION file holds the marketing version as ``MAJ.MIN`` (optionally
    suffixed with ``-prerelease``). The MSI ProductVersion BUILD slot is
    populated separately from ``CI_PIPELINE_IID`` so dev builds upgrade
    cleanly over each other.

    Returns ``(major, minor, prerelease)`` as strings. ``prerelease`` is the
    suffix after the hyphen (e.g., ``"dev"``) or ``""`` if absent.
    """
    with open(version_file) as f:
        raw = f.read().strip()
    m = re.fullmatch(r'([0-9]+)\.([0-9]+)(?:-(.+))?', raw)
    if m:
        return m.group(1), m.group(2), m.group(3) or ""
    raise Exception(f"Could not parse version from {version_file}: '{raw}'")


def version_field(value, name, maximum):
    """Validate and normalize a decimal Windows Installer version field."""
    if not re.fullmatch(r"[0-9]+", value):
        raise ValueError(f"{name} must contain decimal digits, got {value!r}")
    value = value.lstrip("0") or "0"
    if len(value) > len(str(maximum)) or int(value) > maximum:
        raise ValueError(f"{name}={value} exceeds the Windows Installer limit ({maximum})")
    return value


def build_version(version_file=_VERSION_FILE):
    major, minor, pre = parse_version(version_file)
    major = version_field(major, "MAJ_VER", 255)
    minor = version_field(minor, "MIN_VER", 255)
    build = version_field(os.environ.get("CI_PIPELINE_IID") or "0", "CI_PIPELINE_IID", 65535)
    if os.environ.get("IIS_RELEASE_BUILD") == "1":
        pre = ""
    return major, minor, build, pre
