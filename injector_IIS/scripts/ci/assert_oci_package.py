#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""Assert the shape of the datadog-apm-library-iis-rum OCI archive.

The consumer contract lives in the agent's pkg/fleet/installer/oci/download.go:
it looks up the manifest by media type, requires the
com.datadoghq.package.name/.version annotations, and expects exactly one layer
of a specific media type. None of that is checked by `datadog-package create`,
so a change in how we invoke it would only surface as a failed install on a
customer host. Encoded here so CI catches it instead.

Usage: assert_oci_package.py <archive.oci.tar> <expected-name> <expected-version>
"""

import io
import json
import subprocess
import sys
import tarfile

MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
CONFIG_MEDIA_TYPE = "application/vnd.datadog.package.v1"
LAYER_MEDIA_TYPE = "application/vnd.datadog.package.layer.v1.tar+zstd"

EXPECTED_PAYLOAD = {
    "library/injector_IIS.dll",
    "installer/RUMTools.ps1",
    "modules/Datadog.RUM/Datadog.RUM.psm1",
    "modules/Datadog.RUM/Datadog.RUM.psd1",
    "schema/RUM_schema.xml",
}


class Failure(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def blob(tar, digest):
    """Read a blob out of the OCI layout by digest."""
    algorithm, hex_digest = digest.split(":", 1)
    member = tar.extractfile(f"blobs/{algorithm}/{hex_digest}")
    check(member is not None, f"missing blob {digest}")
    return member.read()


def resolve_manifest(tar, index):
    """Follow index.json down to the image manifest, asserting the platform."""
    check(len(index["manifests"]) == 1, "expected exactly one top-level descriptor")
    descriptor = index["manifests"][0]

    if descriptor["mediaType"] == INDEX_MEDIA_TYPE:
        inner = json.loads(blob(tar, descriptor["digest"]))
        check(len(inner["manifests"]) == 1, "expected exactly one manifest in the index")
        descriptor = inner["manifests"][0]

    platform = descriptor.get("platform", {})
    check(
        (platform.get("os"), platform.get("architecture")) == ("windows", "amd64"),
        f"expected platform windows/amd64, got {platform}",
    )
    check(
        descriptor["mediaType"] == MANIFEST_MEDIA_TYPE,
        f"expected manifest media type {MANIFEST_MEDIA_TYPE}, "
        f"got {descriptor['mediaType']}",
    )
    return json.loads(blob(tar, descriptor["digest"])), descriptor["digest"]


def main(argv):
    if len(argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    archive, expected_name, expected_version = argv[1:]

    with tarfile.open(archive) as tar:
        index_json = tar.extractfile("index.json")
        check(index_json is not None, "archive has no index.json -- not an OCI layout")
        index = json.loads(index_json.read())
        manifest, _digest = resolve_manifest(tar, index)

        annotations = manifest.get("annotations", {})
        check(
            annotations.get("com.datadoghq.package.name") == expected_name,
            f"com.datadoghq.package.name is {annotations.get('com.datadoghq.package.name')!r}, "
            f"expected {expected_name!r}",
        )
        check(
            annotations.get("com.datadoghq.package.version") == expected_version,
            f"com.datadoghq.package.version is "
            f"{annotations.get('com.datadoghq.package.version')!r}, "
            f"expected {expected_version!r}",
        )

        check(
            manifest["config"]["mediaType"] == CONFIG_MEDIA_TYPE,
            f"config media type is {manifest['config']['mediaType']}, "
            f"expected {CONFIG_MEDIA_TYPE}",
        )

        layers = manifest["layers"]
        check(len(layers) == 1, f"expected exactly one layer, got {len(layers)}")
        check(
            layers[0]["mediaType"] == LAYER_MEDIA_TYPE,
            f"layer media type is {layers[0]['mediaType']}, expected {LAYER_MEDIA_TYPE}",
        )

        compressed = blob(tar, layers[0]["digest"])

    decompressed = subprocess.run(
        ["zstd", "-d", "-c"], input=compressed, stdout=subprocess.PIPE, check=True
    ).stdout

    with tarfile.open(fileobj=io.BytesIO(decompressed)) as layer:
        names = {name.removeprefix("./") for name in layer.getnames()}

    missing = EXPECTED_PAYLOAD - names
    check(not missing, f"layer is missing {sorted(missing)}; it holds {sorted(names)}")

    print(f"OK: {archive} matches the installer's expected package shape")
    print(f"  name    {expected_name}")
    print(f"  version {expected_version}")
    for path in sorted(EXPECTED_PAYLOAD):
        print(f"  payload {path}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except Failure as failure:
        print(f"OCI package shape assertion failed: {failure}", file=sys.stderr)
        sys.exit(1)
