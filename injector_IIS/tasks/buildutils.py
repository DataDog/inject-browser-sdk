# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""
Miscellaneous build helper functions, no tasks here
"""
from __future__ import print_function

import datetime
import os

from invoke.exceptions import Exit
from .utils import parse_version


def do_get_build_env(ctx, vstudio_root=None):
    build_maj, build_min, pre = parse_version()
    if os.environ.get("IIS_RELEASE_BUILD") == "1":
        pre = ""
    # The BUILD slot of the MSI ProductVersion is CI_PIPELINE_IID at build
    # time. Locally that env var is unset, so we fall back to "0".
    # Windows Installer caps each ProductVersion field at 65535, so fail
    # loudly here rather than letting WiX produce an invalid MSI.
    build_build = os.environ.get("CI_PIPELINE_IID", "0")
    try:
        if int(build_build) > 65535:
            raise Exit(
                f"CI_PIPELINE_IID={build_build} exceeds the Windows Installer "
                "ProductVersion field limit (65535). Switch the BUILD slot to a "
                "bounded counter before pipelines cross this threshold.",
                code=2,
            )
    except ValueError:
        raise Exit(f"CI_PIPELINE_IID must be an integer, got {build_build!r}", code=2)
    numeric = f"{build_maj}.{build_min}.{build_build}"
    fullver = f"{numeric}-{pre}" if pre else numeric

    env = {
        "MAJ_VER": build_maj,
        "MIN_VER": build_min,
        "BUILD_VER": build_build,
        "VERSION_PRE": pre,
    }

    vsroot = os.getenv("VSINSTALLDIR")
    if not vsroot:
        print("Visual Studio Not installed in environment; checking other locations")

        vsroot = vstudio_root or os.getenv("VSTUDIO_ROOT")
        if not vsroot:
            print("Must have visual studio installed")
            raise Exit(code=2)

    batchfile = "VsDevCmd.bat"
    vs_env_bat = "{}\\Common7\\Tools\\{}".format(vsroot, batchfile)

    return env, fullver, vs_env_bat
