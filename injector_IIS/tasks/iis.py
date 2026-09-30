# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""
IIS namespaced tasks
"""
from __future__ import print_function
import codecs
import hashlib
import json
import os
import re
import shutil
import sys

from invoke import task
from invoke.exceptions import Exit
from .buildutils import do_get_build_env
from .utils import parse_version


installer_proj_file = os.path.join(
    "injector_IIS_installer", "injector_IIS_installer.wixproj"
)

PROJECT_DIR = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))

# The fleet-automation OCI package. Registered agent-side in
# pkg/fleet/installer/default_packages.go; installed under
# C:\ProgramData\Datadog\Installer\packages\<name>\<version>\.
OCI_PACKAGE_NAME = "datadog-apm-library-iis-rum"

# (name as emitted by iis.package, path inside the package layer). Everything the
# MSI installs, grouped into subfolders. The Datadog.RUM folder name must equal
# the module name or Import-Module / PSModulePath won't find it.
OCI_PAYLOAD = [
    ("injector_IIS.dll", "library/injector_IIS.dll"),
    ("RUMTools.ps1", "installer/RUMTools.ps1"),
    ("Datadog.RUM.psm1", "modules/Datadog.RUM/Datadog.RUM.psm1"),
    ("Datadog.RUM.psd1", "modules/Datadog.RUM/Datadog.RUM.psd1"),
    ("RUM_schema.xml", "schema/RUM_schema.xml"),
]

# Everything in OCI_PAYLOAD except RUM_schema.xml, which is neither a PE nor a
# PowerShell script and so has nothing for dd-wcs to attach a signature block
# to. It is the one unsigned file in the package.
OCI_UNSIGNABLE = {"RUM_schema.xml"}

OCI_SIGNATURE_MANIFEST = "oci-signatures.json"


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

    # Stamp before signing: the signature block is appended to the file, so any
    # later edit invalidates it.
    stamp_module_version(os.path.join(".", "resources", "modules", "Datadog.RUM.psd1"))

    sign_binary(ctx, os.path.join(".", "resources", "modules", "Datadog.RUM.psm1"))
    sign_binary(ctx, os.path.join(".", "resources", "modules", "Datadog.RUM.psd1"))
    # Matters more for the OCI package than for the MSI: the fleet-installed copy
    # may be invoked by an install hook rather than by an admin at a prompt.
    sign_binary(ctx, os.path.join(".", "resources", "RUMTools.ps1"))

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

    sign_binary(ctx, msipath)

    ## copy the newly built file into the target directory
    target_filename = "injector_IIS_installer.msi"
    shutil.copy2(msipath, os.path.join(out_dir, target_filename))

    # Stage the OCI payload alongside the MSI. iis.oci runs on Linux and cannot
    # run dd-wcs, so every signed file it packages has to originate here.
    for name in (
        os.path.join(".", "resources", "RUMTools.ps1"),
        os.path.join(".", "resources", "modules", "Datadog.RUM.psm1"),
        os.path.join(".", "resources", "modules", "Datadog.RUM.psd1"),
        os.path.join(".", "resources", "RUM_schema.xml"),
    ):
        dest = os.path.join(out_dir, os.path.basename(name))
        print(f"Copy {name} to {dest}")
        shutil.copy2(name, dest)

    # Verify with Windows' Authenticode implementation, then bind the Linux
    # packaging gate to those exact bytes. Signature markers alone do not
    # detect a payload modified after signing.
    manifest = os.path.join(out_dir, OCI_SIGNATURE_MANIFEST)
    if os.path.exists(manifest):
        os.remove(manifest)
    if os.environ.get("SIGN_WINDOWS_DD_WCS"):
        verifier = os.path.join(PROJECT_DIR, "scripts", "ci", "verify-oci-signatures.ps1")
        ctx.run(
            f'powershell -NoProfile -ExecutionPolicy Bypass -File "{verifier}" '
            f'-ArtifactsDir "{os.path.abspath(out_dir)}"',
            hide=False,
            pty=False,
        )


def stamp_module_version(psd1_path):
    """Rewrite the PowerShell module's ModuleVersion from the VERSION file.

    Two package versions can coexist under Installer\\packages\\<name>\\<version>\\,
    so a module that reports a hardcoded version from both is confusing.
    """
    base, _prerelease = parse_version()

    with open(psd1_path, "rb") as f:
        raw = f.read()

    # The manifest ships as UTF-16LE with a BOM. Round-trip whatever encoding it
    # is in: rewriting it as plain UTF-8 would change how PowerShell reads it.
    for bom, encoding in (
        (codecs.BOM_UTF16_LE, "utf-16-le"),
        (codecs.BOM_UTF16_BE, "utf-16-be"),
        (codecs.BOM_UTF8, "utf-8"),
    ):
        if raw.startswith(bom):
            break
    else:
        bom, encoding = b"", "utf-8"

    content = raw[len(bom) :].decode(encoding)
    updated, count = re.subn(
        r"^(\s*ModuleVersion\s*=\s*)'[^']*'",
        lambda m: f"{m.group(1)}'{base}'",
        content,
        count=1,
        flags=re.MULTILINE,
    )
    if count != 1:
        print(f"Could not find ModuleVersion in {psd1_path}")
        raise Exit(code=1)

    with open(psd1_path, "wb") as f:
        f.write(bom + updated.encode(encoding))
    print(f"Stamped ModuleVersion = '{base}' in {psd1_path}")


def sign_binary(ctx, path):
    if os.environ.get("SIGN_WINDOWS_DD_WCS"):
        print(f"Signing {path} with dd-wcs")
        escaped_path = path.replace('"', '""')
        ctx.run(f'dd-wcs sign --replace-signature "{escaped_path}"', hide=False, pty=False)
    else:
        print(f"Skipping code signing for {path} (SIGN_WINDOWS_DD_WCS not set)")


@task
def oci_version(ctx, version=None, allow_prerelease=False):
    """
    Print the OCI package version for the current VERSION file.
    """
    print(resolve_oci_version(version, allow_prerelease=allow_prerelease))


def resolve_oci_version(version=None, allow_prerelease=False):
    """Resolve the package version, as the X.Y.Z-N shape the installer's
    apmLanguageVersion resolves to.

    A release VERSION yields `X.Y.Z-1`. A prerelease one is only accepted with
    `allow_prerelease`, and then yields `X.Y.Z-0.dev` -- deliberately distinct so
    it can never be mistaken for a release. CI builds pass the flag so the
    packaging path and its shape assertion run on every change; publishing never
    does, because there is no apmPackageDefaultVersions pin for iis-rum and
    `latest` is what an opted-in customer without a pin receives.
    """
    if version:
        return version
    base, prerelease = parse_version()
    if not prerelease:
        return f"{base}-1"
    if allow_prerelease:
        return f"{base}-0.dev"
    raise Exit(
        f"Refusing to use prerelease version '{base}-{prerelease}'. Bump VERSION "
        f"to a release, pass --allow-prerelease to build a non-publishable "
        f"package, or pass --version explicitly.",
        code=1,
    )


# The package-version shape resolve_oci_version produces for a release: an
# X.Y.Z base and a package revision of 1 or more. Matched positively so that a
# --version passed by hand has to be a release too -- a negative test would let
# `1.2.1-dev`, `1.2.1-rc.1` or a typo through to the registry.
RELEASE_VERSION_RE = re.compile(r"^\d+\.\d+\.\d+-[1-9]\d*$")


def is_publishable_version(version):
    """Whether `version` is a release version rather than a dev build."""
    return bool(RELEASE_VERSION_RE.fullmatch(version))


@task
def oci(
    ctx,
    artifacts_dir="../artifacts",
    out_dir="../oci-out",
    version=None,
    allow_prerelease=False,
    allow_unsigned=False,
):
    """
    Build the datadog-apm-library-iis-rum OCI package from an existing build.

    Runs on Linux: datadog-package is a Linux binary and the payload comes off a
    Windows runner already Authenticode-signed. Never rebuilds the DLL.

    Signatures are verified by default. `--allow-unsigned` is the escape hatch
    for packaging a locally-built, unsigned payload; CI must never pass it.
    """
    package_version = resolve_oci_version(version, allow_prerelease=allow_prerelease)

    artifacts_dir = os.path.abspath(artifacts_dir)
    out_dir = os.path.abspath(out_dir)
    os.makedirs(out_dir, exist_ok=True)

    staging = os.path.join(out_dir, "staging")
    shutil.rmtree(staging, ignore_errors=True)

    # Deliberately not keyed off SIGN_WINDOWS_DD_WCS: that is set inside the
    # Windows build container and does not travel with its artifacts, so keying
    # off it would disable the check in exactly the Linux job it exists to
    # guard. Verify unless the caller explicitly opted out.
    require_signatures = not allow_unsigned
    verified_hashes = {}
    if require_signatures:
        try:
            with open(os.path.join(artifacts_dir, OCI_SIGNATURE_MANIFEST)) as f:
                verified_hashes = json.load(f)
        except (OSError, ValueError) as error:
            raise Exit(
                f"Missing or invalid Windows Authenticode verification manifest: {error}",
                code=1,
            ) from error
        if not isinstance(verified_hashes, dict):
            raise Exit("Invalid Windows Authenticode verification manifest", code=1)
    for name, layer_path in OCI_PAYLOAD:
        source = os.path.join(artifacts_dir, name)
        if not os.path.exists(source):
            raise Exit(
                f"Missing OCI payload file '{name}' in {artifacts_dir}. It must be "
                f"produced by iis.package and carried through by "
                f"build_injector_IIS.bat.",
                code=1,
            )
        if require_signatures and name not in OCI_UNSIGNABLE:
            assert_signed(source, verified_hashes)
        dest = os.path.join(staging, *layer_path.split("/"))
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copy2(source, dest)
        print(f"Staged {name} -> {layer_path}")

    archive = os.path.join(
        out_dir, f"{OCI_PACKAGE_NAME}-{package_version}-windows-amd64.oci.tar"
    )
    ctx.run(
        "datadog-package create"
        f" --version {package_version}"
        f" --package {OCI_PACKAGE_NAME}"
        " --os windows --arch amd64 --archive"
        f" --archive-path {archive}"
        f" {staging}/",
        hide=False,
        pty=False,
    )

    shutil.rmtree(staging, ignore_errors=True)
    print(f"Built {archive}")


def assert_signed(path, verified_hashes):
    """Require the exact payload verified by the trusted Windows build job."""
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if verified_hashes.get(os.path.basename(path)) != digest:
        raise Exit(
            f"{path} does not match the Windows Authenticode-verified payload. "
            "Refusing to package it. Use --allow-unsigned only for a local build.",
            code=1,
        )


@task
def oci_push(
    ctx,
    oci_dir="../oci-out",
    registry="registry.ddbuild.io/ci/remote-updates",
    version=None,
    extra_tag=None,
):
    """
    Push the OCI package built by iis.oci to a registry.

    Note the naming split: the registry repository uses the package name, while
    the public `apm-library-iis-rum-package` name is the S3 mirror produced by
    `datadog-package replicate-s3` from the agent release pipeline.
    """
    # Deliberately no allow_prerelease passthrough: a dev build must never be
    # pushed, so resolving refuses a prerelease VERSION outright here.
    package_version = resolve_oci_version(version)
    if not is_publishable_version(package_version):
        raise Exit(
            f"Refusing to push non-publishable version '{package_version}'; "
            f"expected a release package version of the form X.Y.Z-N.",
            code=1,
        )
    archive = os.path.abspath(
        os.path.join(
            oci_dir, f"{OCI_PACKAGE_NAME}-{package_version}-windows-amd64.oci.tar"
        )
    )
    if not os.path.exists(archive):
        raise Exit(f"Missing {archive}; run iis.oci first.", code=1)

    tags = [package_version]
    if extra_tag:
        tags.append(extra_tag)
    for tag in tags:
        ctx.run(
            f"datadog-package push {registry}/{OCI_PACKAGE_NAME}:{tag} {archive}",
            hide=False,
            pty=False,
        )
        print(f"Pushed {registry}/{OCI_PACKAGE_NAME}:{tag}")
