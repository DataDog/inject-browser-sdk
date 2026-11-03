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
import sys
from typing import NamedTuple


# Path to the IIS injector VERSION file
_VERSION_FILE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.realpath(__file__))), "VERSION"
)

# Warn well before CI_PIPELINE_IID overflows the 16-bit MSI BUILD field.
BUILD_WARNING_THRESHOLD = 60000


def parse_version(version_file=_VERSION_FILE):
    """Return the effective numeric version and prerelease suffix.

    Preserve the two-value API used by package versions and module stamps.
    The numeric version is MAJ.MIN.BUILD, including the pipeline IID (or 0
    locally), so those consumers agree with the DLL and MSI metadata.
    """
    v = build_version(version_file)
    return f"{v.major}.{v.minor}.{v.build}", v.pre


class BuildVersion(NamedTuple):
    major: str
    minor: str
    build: str
    pre: str
    release: str  # release-please's MAJ.MIN.PATCH from VERSION

    @property
    def display(self):
        """Human-readable version: the release, or a prerelease of it."""
        return f"{self.release}-{self.pre}.{self.build}" if self.pre else self.release


def _parse_release_version(version_file):
    """Parse the IIS injector release version from its VERSION file.

    release-please owns VERSION and writes the release as ``MAJ.MIN.PATCH``
    (optionally suffixed with ``-prerelease``). The MSI ProductVersion is
    ``MAJ.MIN.BUILD``: Windows Installer has no room for PATCH next to the
    ``CI_PIPELINE_IID`` that keeps every build upgradable, so PATCH only
    appears in the display version.

    Returns ``(major, minor, patch, prerelease)`` as strings. ``prerelease``
    is the suffix after the hyphen, or ``"dev"`` when absent.
    """
    with open(version_file) as f:
        raw = f.read().strip()
    m = re.fullmatch(r'([0-9]+)\.([0-9]+)\.([0-9]+)(?:-(.+))?', raw)
    if m:
        return m.group(1), m.group(2), m.group(3), m.group(4) or "dev"
    raise ValueError(f"Could not parse version from {version_file}: '{raw}'")


def version_field(value, name, maximum):
    """Validate and normalize a decimal Windows Installer version field."""
    if not re.fullmatch(r"[0-9]+", value):
        raise ValueError(f"{name} must contain decimal digits, got {value!r}")
    value = value.lstrip("0") or "0"
    if len(value) > len(str(maximum)) or int(value) > maximum:
        raise ValueError(f"{name}={value} exceeds the Windows Installer limit ({maximum})")
    return value


def build_version(version_file=_VERSION_FILE):
    major, minor, patch, pre = _parse_release_version(version_file)
    major = version_field(major, "MAJ_VER", 255)
    minor = version_field(minor, "MIN_VER", 255)
    patch = version_field(patch, "PATCH", 65535)
    build = version_field(os.environ.get("CI_PIPELINE_IID") or "0", "CI_PIPELINE_IID", 65535)
    if int(build) > BUILD_WARNING_THRESHOLD:
        print(
            f"warning: CI_PIPELINE_IID={build} is close to the Windows Installer "
            "limit (65535); plan a bounded BUILD counter",
            file=sys.stderr,
        )
    # Only CI sets this, for release-please branches and iis-* tags.
    if os.environ.get("IIS_RELEASE_BUILD") == "1":
        pre = ""
    return BuildVersion(major, minor, build, pre, f"{major}.{minor}.{patch}")
