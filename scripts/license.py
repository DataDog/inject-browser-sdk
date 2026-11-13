#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

"""Generate/check attribution from tracked source, Cargo metadata and manual entries."""

import argparse
import csv
import difflib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
FIELDS = ("Component", "Origin", "License", "Copyright")
# These contain attribution data, rather than copies of third-party source.
INVENTORIES = {"LICENSE-3rdparty.csv", "licenses/manual.csv", "licenses/source-files.json"}


def read_csv(path):
    with path.open(newline="", encoding="utf-8") as stream:
        reader = csv.DictReader(stream)
        if reader.fieldnames != list(FIELDS):
            raise ValueError(f"{path}: expected columns {FIELDS}")
        rows = list(reader)
    for row in rows:
        if set(row) != set(FIELDS) or any(not isinstance(value, str) or not value.strip() for value in row.values()):
            raise ValueError(f"{path}: incomplete attribution: {row}")
    return rows


def render(rows):
    stream = io.StringIO(newline="")
    writer = csv.writer(stream, quoting=csv.QUOTE_ALL, lineterminator="\n")
    writer.writerow(FIELDS)
    writer.writerows(sorted({tuple(row[field] for field in FIELDS) for row in rows}))
    return stream.getvalue()


def notices(files):
    return sorted({notice["copyright"] for file in files for notice in file.get("copyrights", [])})


def expressions(files):
    found = {
        detection["license_expression_spdx"]
        for file in files for detection in file.get("license_detections", [])
        if detection.get("license_expression_spdx")
    }
    # ScanCode interprets "Unless explicitly stated otherwise" in our header
    # as an unknown reference alongside the explicitly named Apache license.
    boilerplate = "LicenseRef-scancode-unknown-license-reference AND Apache-2.0"
    return sorted({"Apache-2.0" if value == boilerplate else value for value in found})


def rust_records(packages, scans):
    rows = []
    for index, package in enumerate(packages):
        origin = package.get("repository") or package.get("homepage")
        license_expression = package.get("license")
        if not origin or not license_expression:
            raise ValueError(f"{package['name']}: missing Cargo origin/license; review upstream metadata")
        copyrights = notices([file for path, file in scans.items() if path.startswith(f"rust/{index}/")])
        # Match Datadog's rust-license-tool fallback when upstream omits notices.
        copyright_text = "; ".join(copyrights) or ", ".join(package["authors"]) or f"The {package['name']} Authors"
        rows.append(dict(zip(FIELDS, (package["name"], origin, license_expression, copyright_text))))
    return rows


def source_records(scans, config):
    owners = {}
    for component, attribution in config["third_party"].items():
        for path in attribution["files"]:
            if path in owners:
                raise ValueError(f"{path}: listed under multiple components")
            owners[path] = component
            if f"source/{path}" not in scans:
                raise ValueError(f"{path}: attributed source file is missing")
    reviewed = config.get("first_party", {})
    for path, review in reviewed.items():
        if path in owners or f"source/{path}" not in scans or not review.get("reason"):
            raise ValueError(f"{path}: invalid first-party review")
    for path, file in scans.items():
        if not path.startswith("source/"):
            continue
        path = path.removeprefix("source/")
        if path in INVENTORIES or path in owners or Path(path).name == "package-lock.json":
            continue
        holders = [holder["holder"] for holder in file.get("holders", [])]
        foreign = [holder for holder in holders if "datadog" not in holder.lower()]
        licenses = expressions([file])
        if path in reviewed:
            if foreign or licenses != sorted(reviewed[path]["licenses"]):
                raise ValueError(f"{path}: reviewed first-party notices changed; review licenses/source-files.json")
            continue
        if foreign or any(license_id != "Apache-2.0" for license_id in licenses):
            raise ValueError(
                f"{path}: unregistered source attribution: holders={foreign}, licenses={licenses}. "
                "Review and register third-party files in licenses/source-files.json."
            )
    rows = []
    for component, attribution in config["third_party"].items():
        files = [scans[f"source/{path}"] for path in attribution["files"]]
        licenses, copyrights = expressions(files), notices(files)
        if not licenses or not copyrights:
            raise ValueError(f"{component}: source scan did not find both a license and copyright")
        rows.append(dict(zip(FIELDS, (component, attribution["origin"], " AND ".join(f"({license_id})" for license_id in licenses) if len(licenses) > 1 else licenses[0], "; ".join(copyrights)))))
    return rows


def tracked_files(root):
    result = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root, check=True, capture_output=True,
    )
    return sorted(set(os.fsdecode(result.stdout).strip("\0").split("\0")))


def scan(root, packages, paths, stage, report):
    source_paths = {}
    expected = set()
    for index, name in enumerate(paths):
        source = root / name
        if source.is_symlink() and not source.resolve().is_relative_to(root):
            raise ValueError(f"{name}: symlink points outside the repository")
        if source.is_file():
            # Neutral names ensure ScanCode also scans VCS metadata (such as
            # .gitignore), which its inventory would otherwise always skip.
            staged_name = f"source/{index}{source.suffix}"
            source_paths[staged_name] = f"source/{name}"
            expected.add(f"source/{name}")
            target = stage / staged_name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
    for index, package in enumerate(packages):
        directory = Path(package["manifest_path"]).parent
        candidates = {
            path for path in directory.iterdir()
            if path.is_file() and path.name.lower().startswith(("license", "copying", "copyright", "notice", "readme"))
        }
        if package.get("license_file"):
            candidates.add(directory / package["license_file"])
        for number, source in enumerate(sorted(candidates)):
            target = stage / "rust" / str(index) / f"{number}-{source.name}"
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            expected.add(target.relative_to(stage).as_posix())
    environment = os.environ.copy()
    environment.setdefault("SCANCODE_CACHE", str(root / ".cache" / "scancode"))
    subprocess.run(
        ["scancode", "--license", "--copyright", "--strip-root", "--processes", "2", "--json-pp", str(report), str(stage)],
        check=True, env=environment, stdout=sys.stderr,
    )
    result = json.loads(report.read_text())
    files = {
        source_paths.get(file["path"], file["path"]): file
        for file in result["files"] if file["type"] == "file"
    }
    for path, file in files.items():
        if file.get("scan_errors"):
            raise ValueError(f"{path}: scan failed: {file['scan_errors']}")
    if expected - files.keys():
        raise ValueError(f"ScanCode skipped files: {sorted(expected - files.keys())}")
    return files


def generate(root):
    manual = read_csv(root / "licenses/manual.csv")
    config = json.loads((root / "licenses/source-files.json").read_text())
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
        cwd=root, check=True, capture_output=True, text=True,
    ).stdout)
    # No target filter: include Windows/Wasm as well as build and test dependencies.
    packages = sorted(
        (package for package in metadata["packages"] if package["source"] is not None),
        key=lambda package: (package["name"], package["version"], package["source"]),
    )
    with tempfile.TemporaryDirectory(prefix="license-check-") as directory:
        directory = Path(directory)
        stage = directory / "input"
        scans = scan(root, packages, tracked_files(root), stage, directory / "scan.json")
        rows = rust_records(packages, scans) + source_records(scans, config)
    automatic_names = {row["Component"] for row in rows}
    collisions = automatic_names & {row["Component"] for row in manual}
    if collisions:
        raise ValueError(f"Remove automatically generated components from licenses/manual.csv: {sorted(collisions)}")
    return render(manual + rows)


def write_inventory(target, content):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=target.parent,
            prefix=f".{target.name}.", delete=False,
        ) as stream:
            temporary = Path(stream.name)
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.chmod(target.stat().st_mode & 0o777 if target.exists() else 0o644)
        os.replace(temporary, target)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true", help="Update LICENSE-3rdparty.csv instead of checking it")
    mode.add_argument("--licenses-script", metavar="PATH", help="Deprecated: emit CSV to stdout; the external script is no longer used")
    args = parser.parse_args()
    if args.licenses_script is not None:
        print("--licenses-script is deprecated; use --write to update the inventory. The external script is no longer used.", file=sys.stderr)
    try:
        expected = generate(ROOT)
        if args.licenses_script is not None:
            sys.stdout.write(expected)
            return 0
        target = ROOT / "LICENSE-3rdparty.csv"
        if args.write:
            write_inventory(target, expected)
            print("Updated LICENSE-3rdparty.csv")
            return 0
        actual = target.read_text(encoding="utf-8") if target.exists() else ""
        if actual != expected:
            sys.stdout.writelines(difflib.unified_diff(actual.splitlines(True), expected.splitlines(True), fromfile=str(target.name), tofile="generated"))
            print("Run python scripts/license.py --write and commit LICENSE-3rdparty.csv.", file=sys.stderr)
            return 1
        print("LICENSE-3rdparty.csv is up to date (repository source and all Rust dependencies).")
        return 0
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"License check failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
