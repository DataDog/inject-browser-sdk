#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.
"""
Install dependencies for cross-compilation.

Usage:
======
./install-xwin-toolchain.py --output-dir /xwin
"""
import argparse
import json
import typing
import subprocess
import sys
import platform
import tempfile
import tarfile
import urllib.request
import shutil
import hashlib
import zipfile
from pathlib import Path

XWIN_VERSION = "0.6.5"


def download_file(url: str, destination: Path) -> Path:
    print(f"Downloading {url} to {destination}")
    response = urllib.request.urlopen(url)
    with open(destination, "wb") as output:
        shutil.copyfileobj(response, output)

    assert destination.exists()
    return destination


def read_content_file(p: Path) -> str:
    with open(p, "r") as f:
        return f.readlines()[0].strip()


def sha256(filename: Path) -> str:
    sha256_hash = hashlib.sha256()
    with open(filename, "rb") as f:
        for byte_block in iter(lambda: f.read(4096), b""):
            sha256_hash.update(byte_block)
    return sha256_hash.hexdigest()


def build_url(os: str, arch: str, xwin_version: str) -> tuple[str, str]:
    normalize_arch = {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "amd64": "x86_64",
        "x86_64": "x86_64",
    }

    normalize_os = {
        "Linux": "unknown-linux-musl",
        "Darwin": "apple-darwin",
        "Windows": "pc-windows-msvc",
    }

    final_os = normalize_os.get(os, None)
    final_arch = normalize_arch.get(arch, None)

    assert final_os
    assert final_arch

    targz = f"https://github.com/Jake-Shadle/xwin/releases/download/{xwin_version}/xwin-{xwin_version}-{final_arch}-{final_os}.tar.gz"
    sign = targz + ".sha256"
    return targz, sign


def find_bin_location(root: Path, bin: str) -> typing.Optional[Path]:
    for p in root.glob(f"**/{bin}"):
        return root.joinpath(p)

    return None


def run(cmd: str) -> int:
    print(f"Running {cmd}")
    process = subprocess.Popen(
        cmd,
        shell=True,
        stdout=subprocess.PIPE,
        text=True,  # This allows us to read the output as strings
    )

    if process.stdout:
        for line in iter(process.stdout.readline, ""):
            sys.stdout.write(line)

    process.wait()
    return process.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description="Install windows toolchain")
    parser.add_argument(
        "--xwin-version",
        help=f"xwin version to download. Default to {XWIN_VERSION}",
        default=XWIN_VERSION,
    )
    parser.add_argument(
        "--skip-verification", help="Skip signature verification", action="store_true"
    )
    parser.add_argument(
        "--output-dir", help="Location where the toolchain will be installed"
    )
    parser.add_argument(
        "--skip-asan",
        help="Skip fetching MSVC ASan VSIX packages",
        action="store_true",
    )
    args = parser.parse_args()

    os = platform.system()
    arch = platform.machine()

    artifact_url, signature_url = build_url(os, arch, args.xwin_version)

    with tempfile.TemporaryDirectory() as work_dir:
        p_work_dir = Path(work_dir)
        xwin_tgz = download_file(artifact_url, p_work_dir.joinpath("xwin.tar.gz"))

        if not args.skip_verification:
            xwin_sign = download_file(signature_url, p_work_dir.joinpath("xwin.sha256"))

            expected_sha256 = read_content_file(xwin_sign)
            computed_sha256 = sha256(xwin_tgz)
            if expected_sha256 != computed_sha256:
                print(
                    f"sha256 verification failed. expected={expected_sha256}, computed={computed_sha256}"
                )
                return 1

        with tarfile.open(xwin_tgz) as tar:
            tar.extractall(path=work_dir)

        xwin_bin = find_bin_location(p_work_dir, "xwin")
        if not xwin_bin:
            print(f'Could not find "{xwin_bin}" binary in {p_work_dir}')
            return 1

        cmd = f"{xwin_bin} --accept-license splat --include-debug-libs --include-debug-symbols --output {args.output_dir}"
        rc = run(cmd)
        if rc != 0:
            return rc

        # xwin's package filter excludes the MSVC ASan VSIX packages, so the
        # x86_64 sanitizer libs/headers (clang_rt.asan*) aren't part of the
        # splatted output. Fetch them manually from Microsoft's VS catalog.
        if not args.skip_asan:
            install_msvc_asan(Path(args.output_dir), p_work_dir)
        return 0


VS_CHANNEL_MANIFEST_URL = "https://aka.ms/vs/17/release/channel"


def install_msvc_asan(output_dir: Path, work_dir: Path) -> None:
    """Download and splat the MSVC ASan VSIX packages into the xwin output.

    xwin doesn't include ASan in its default package set, so we resolve the
    latest `Microsoft.VC.<ver>.ASAN.{X64,Headers}.base` packages from the VS
    Build Tools manifest, download the VSIX zips, and copy
      - `Contents/VC/Tools/MSVC/<ver>/lib/x64/clang_rt.asan*.lib`
      - `Contents/VC/Tools/MSVC/<ver>/include/sanitizer/*`
    into the matching `crt/lib/x86_64/` and `crt/include/` paths xwin uses.
    """
    print("Resolving MSVC ASan packages from VS catalog...")
    channel_path = work_dir / "vs_channel.json"
    download_file(VS_CHANNEL_MANIFEST_URL, channel_path)
    with open(channel_path) as f:
        channel = json.load(f)
    vsman_url = next(
        pl["url"]
        for ci in channel["channelItems"]
        if ci.get("id") == "Microsoft.VisualStudio.Manifests.VisualStudio"
        for pl in ci.get("payloads", [])
        if pl.get("fileName", "").endswith(".vsman")
    )
    vsman_path = work_dir / "vs.vsman"
    download_file(vsman_url, vsman_path)
    with open(vsman_path) as f:
        vsman = json.load(f)

    asan_x64 = max(
        (p for p in vsman["packages"] if p["id"].endswith(".ASAN.X64.base")),
        key=lambda p: p["id"],
    )
    asan_headers = max(
        (p for p in vsman["packages"] if p["id"].endswith(".ASAN.Headers.base")),
        key=lambda p: p["id"],
    )
    print(f"Using {asan_x64['id']} and {asan_headers['id']}")

    crt_lib = output_dir / "crt" / "lib" / "x86_64"
    crt_include = output_dir / "crt" / "include"
    crt_lib.mkdir(parents=True, exist_ok=True)
    crt_include.mkdir(parents=True, exist_ok=True)

    # (pkg, [(path-substring matched against VSIX entry, dest dir, basename predicate)])
    rules: list[tuple[dict, list[tuple[str, Path, typing.Callable[[str], bool]]]]] = [
        (
            asan_x64,
            [
                ("/lib/x64/", crt_lib, lambda n: n.startswith("clang_rt.asan")),
                ("/bin/Hostx64/x64/", crt_lib, lambda n: n.startswith("clang_rt.asan") and n.endswith(".dll")),
            ],
        ),
        (asan_headers, [("/include/sanitizer/", crt_include / "sanitizer", lambda n: True)]),
    ]
    for pkg, patterns in rules:
        for payload in pkg.get("payloads", []):
            if not payload.get("fileName", "").endswith(".vsix"):
                continue
            vsix_path = work_dir / payload["fileName"]
            download_file(payload["url"], vsix_path)
            with zipfile.ZipFile(vsix_path) as zf:
                for member in zf.namelist():
                    if member.endswith("/"):
                        continue
                    for needle, dest_root, predicate in patterns:
                        idx = member.find(needle)
                        if idx == -1:
                            continue
                        rel = member[idx + len(needle):]
                        if not predicate(Path(rel).name):
                            continue
                        target = dest_root / rel
                        target.parent.mkdir(parents=True, exist_ok=True)
                        with zf.open(member) as src, open(target, "wb") as dst:
                            shutil.copyfileobj(src, dst)
                        break


if __name__ == "__main__":
    sys.exit(main())
