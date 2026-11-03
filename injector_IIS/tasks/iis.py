# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""
IIS namespaced tasks
"""
from __future__ import print_function
import os
import shutil
import sys

from invoke import task
from invoke.exceptions import Exit
from .buildutils import do_get_build_env


installer_proj_file = os.path.join(
    "injector_IIS_installer", "injector_IIS_installer.wixproj"
)

PROJECT_DIR = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))


@task
def build(ctx, out_dir="../build_out", debug=False, vstudio_root=None, test_dir=None):
    """
    Build the IIS RUM module
    """

    if sys.platform != "win32":
        print(
            "This task builds the IIS RUM injector for windows, and must be built on Windows\n"
        )
        raise Exit(code=1)

    if not os.path.exists(out_dir):
        os.mkdir(out_dir)

    if not test_dir:
        test_dir = os.path.join(PROJECT_DIR, "test")

    # NOTE(@dmehala): Escape path
    test_dir = test_dir.replace("\\", "/")

    env, fullver, vs_env_bat = do_get_build_env(ctx, vstudio_root=vstudio_root)

    ## Configure and Build the project
    build_type = "Debug" if debug else "RelWithDebInfo"
    cmake_configure_cmd = (
        f"cmake --preset=ci-native -B build -DCMAKE_BUILD_TYPE={build_type} ."
    )

    result = ctx.run(
        f'call "{vs_env_bat}" -arch=amd64 && {cmake_configure_cmd}',
        env=env,
        hide=False,
        pty=False,
    )

    cmake_build_cmd = "cmake --build build -j 4"
    result = ctx.run(
        f'call "{vs_env_bat}" -arch=amd64 && {cmake_build_cmd}',
        env=env,
        hide=False,
        pty=False,
    )

    testpath = os.path.join(
        PROJECT_DIR, "build", "test", "unittests", "iis_injector_tests.exe"
    )
    if not os.path.exists(testpath):
        print(f"Could not find unit tests at {testpath}")
        raise Exit(code=1)
    shutil.copy2(testpath, out_dir)

    dllpath = os.path.join(PROJECT_DIR, "build", "iis_injector.dll")
    if not os.path.exists(dllpath):
        print(f"Could not find the injector DLL at {dllpath}")
        raise Exit(code=1)

    sign_binary(ctx, dllpath)

    package(ctx, out_dir, iis_dll=dllpath, debug=debug, vstudio_root=vstudio_root)


@task
def package(ctx, out_dir="../build_out", iis_dll="", debug=False, vstudio_root=None):
    """
    Build the IIS RUM Installer
    """

    if sys.platform != "win32":
        print(
            "This task builds the IIS RUM injector for windows, and must be built on Windows\n"
        )
        raise Exit(code=1)

    if not os.path.exists(out_dir):
        os.mkdir(out_dir)

    if not iis_dll:
        print("Missing dll path")
        raise Exit(code=1)

    env, fullver, vs_env_bat = do_get_build_env(ctx, vstudio_root=vstudio_root)

    sign_binary(ctx, os.path.join(".", "resources", "modules", "Datadog.RUM.psm1"))
    sign_binary(ctx, os.path.join(".", "resources", "modules", "Datadog.RUM.psd1"))

    # copy the dll
    dllpath = os.path.join(out_dir, "injector_IIS.dll")
    print(f"Copy {iis_dll} to {dllpath}")
    shutil.copy2(iis_dll, dllpath)

    dllpath = dllpath.replace("\\", "/")

    # clean installer obj
    shutil.rmtree(os.path.join("injector_IIS_installer", "obj"), ignore_errors=True)
    shutil.rmtree(os.path.join("injector_IIS_installer", "bin"), ignore_errors=True)

    ## build the installer
    build_type = "Debug" if debug else "Release"
    # NOTE(@dmehala): `OUT_DIR` is a special env var used by msbuild. For some unknown reasons, the task
    # argument is also passed as env var by this framework...
    msi_cmd = f'msbuild {installer_proj_file} -m:4 -p:Configuration={build_type};Platform=x64;InjectorDll="{dllpath}"'

    print(f"Executing: {msi_cmd}")
    result = ctx.run(
        f'call "{vs_env_bat}" -arch=amd64 && {msi_cmd}',
        hide=False,
        pty=False,
        env=env,
    )

    msipath = os.path.join(out_dir, "en-us", "injector_IIS_installer.msi")
    if not os.path.exists(msipath):
        print("Missing installer")
        raise Exit(code=1)

    # Check the actual Windows artifacts before signing/publishing them. The
    # sidecar supplies the build expectation to both install and upgrade E2E.
    verifier = os.path.join(PROJECT_DIR, "scripts", "Assert-IISArtifactVersion.ps1")
    ctx.run(
        f'powershell -NoProfile -ExecutionPolicy Bypass -File "{verifier}" '
        f'-DllPath "{dllpath}" -MsiPath "{msipath}"',
        env=env,
        hide=False,
        pty=False,
    )
    with open(os.path.join(out_dir, "injector_IIS.version"), "w") as version_file:
        version_file.write(f"{env['MAJ_VER']}.{env['MIN_VER']}.{env['BUILD_VER']}\n")
    # The display version, which the release job matches against the tag.
    with open(os.path.join(out_dir, "injector_IIS.release"), "w") as release_file:
        release_file.write(f"{fullver}\n")

    sign_binary(ctx, msipath)
    if os.environ.get("SIGN_WINDOWS_DD_WCS"):
        # Fail the release build rather than publish anything left unsigned.
        signed = [dllpath, msipath] + [
            os.path.join(".", "resources", "modules", name) for name in ("Datadog.RUM.psm1", "Datadog.RUM.psd1")
        ]
        checker = os.path.join(PROJECT_DIR, "scripts", "Assert-IISSignature.ps1")
        paths = " ".join(f'"{path}"' for path in signed)
        ctx.run(f'powershell -NoProfile -ExecutionPolicy Bypass -File "{checker}" {paths}', hide=False, pty=False)

    ## copy the newly built file into the target directory
    target_filename = "injector_IIS_installer.msi"
    shutil.copy2(msipath, os.path.join(out_dir, target_filename))


def sign_binary(ctx, path):
    if os.environ.get("SIGN_WINDOWS_DD_WCS"):
        # windows-code-signer.exe signs with the job's CI Identity AWS credentials,
        # injected as environment variables by the IIS build job.
        print(f"Signing {path} with windows-code-signer")
        escaped_path = path.replace('"', '""')
        ctx.run(
            f'C:/devtools/windows-code-signer.exe sign --replace-signature "{escaped_path}"',
            hide=False,
            pty=False,
        )
    else:
        print(f"Skipping code signing for {path} (SIGN_WINDOWS_DD_WCS not set)")


