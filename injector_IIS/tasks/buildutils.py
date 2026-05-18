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
    numeric, pre = parse_version()
    build_maj, build_min = numeric.split(".")
    # IIS_BUILD_NUMBER lets a developer reproduce a specific CI build's
    # version locally; otherwise BUILD comes from GitLab's per-project IID.
    build_num = os.environ.get("IIS_BUILD_NUMBER") or os.environ.get("CI_PIPELINE_IID", "0")
    full_numeric = f"{build_maj}.{build_min}.{build_num}"
    fullver = f"{full_numeric}-{pre}" if pre else full_numeric

    env = {
        "MAJ_VER": build_maj,
        "MIN_VER": build_min,
        "BUILD_VER": build_num,
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
