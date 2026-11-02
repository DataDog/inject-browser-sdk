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
from .utils import build_version


def do_get_build_env(ctx, vstudio_root=None):
    try:
        build_maj, build_min, build_build, pre = build_version()
    except ValueError as error:
        raise Exit(str(error), code=2) from error
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
