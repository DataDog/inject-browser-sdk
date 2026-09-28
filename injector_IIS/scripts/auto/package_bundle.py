#!/usr/bin/env python3
"""Stage the unsigned x64 IIS bootstrap bundle for signing and release."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
from typing import Optional, Tuple
import zipfile


ASSETS = (
    "Invoke-IisRumLifecycle.ps1",
    "Install-IisRum.ps1",
    "ddiisrum-bootstrap.dll",
    "RUM_schema.xml",
)
VERSION = re.compile(r"[0-9][0-9A-Za-z._+-]{0,127}\Z")


def assert_x64_dll(path: Path) -> None:
    data = path.read_bytes()
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError(f"Not a Windows PE DLL: {path}")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset + 24 > len(data) or data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError(f"Invalid PE header: {path}")
    machine, _, _, _, _, _, characteristics = struct.unpack_from(
        "<HHIIIHH", data, pe_offset + 4
    )
    if machine != 0x8664 or not (characteristics & 0x2000):
        raise ValueError(f"Expected an x64 PE DLL: {path}")


def build_bundle(
    dll: Path, out_dir: Path, version: str, assets_dir: Optional[Path] = None
) -> Tuple[Path, str]:
    if not VERSION.fullmatch(version):
        raise ValueError(
            "Bundle version must be explicit and contain only version characters"
        )
    assert_x64_dll(dll)
    source_dir = assets_dir or Path(__file__).resolve().parent
    manifest = {
        "contract_version": 1,
        "architecture": "x64",
        "runtime_abi": 1,
        "bundle_version": version,
    }
    schema = (
        source_dir / "RUM_schema.xml"
        if assets_dir
        else source_dir.parent.parent / "resources" / "RUM_schema.xml"
    )
    contents = {
        "manifest.json": (json.dumps(manifest, separators=(",", ":")) + "\n").encode(),
        "Invoke-IisRumLifecycle.ps1": (
            source_dir / "Invoke-IisRumLifecycle.ps1"
        ).read_bytes(),
        "Install-IisRum.ps1": (source_dir / "Install-IisRum.ps1").read_bytes(),
        "ddiisrum-bootstrap.dll": dll.read_bytes(),
        "RUM_schema.xml": schema.read_bytes(),
    }
    assert set(contents) == {"manifest.json", *ASSETS}
    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / f"iis-rum-bootstrap-{version}-x64.zip"
    temporary = archive.with_suffix(".zip.tmp")
    try:
        with zipfile.ZipFile(
            temporary, "w", compression=zipfile.ZIP_DEFLATED
        ) as bundle:
            for name, content in contents.items():
                entry = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                bundle.writestr(entry, content)
        temporary.replace(archive)
    finally:
        temporary.unlink(missing_ok=True)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n")
    return archive, digest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bootstrap-dll", required=True, type=Path)
    parser.add_argument("--bundle-version", required=True)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument(
        "--assets-dir", type=Path, help="Directory holding signed scripts and schema"
    )
    args = parser.parse_args()
    archive, digest = build_bundle(
        args.bootstrap_dll, args.out_dir, args.bundle_version, args.assets_dir
    )
    print(f"bundle_version={args.bundle_version}")
    print(f"archive={archive}")
    print(f"sha256={digest}")


if __name__ == "__main__":
    main()
