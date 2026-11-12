#!/usr/bin/env bash
# Called by the one-pipeline package-oci job from packaging/, once per OS/ARCH.
# Fills sources/ with the IIS injector built by injector_IIS_build.
set -euo pipefail

# The IIS injector is windows/amd64 only. package-oci skips an empty sources/.
if [ "${OS:-}" != "windows" ] || [ "${ARCH:-}" != "amd64" ]; then
  echo "No IIS injector for ${OS:-}/${ARCH:-}, skipping"
  rm -rf sources
  exit 0
fi

artifacts=../artifacts
# X.Y.Z on iis-X.Y.Z tags, X.Y.Z-dev.<pipeline IID> otherwise.
version="$(tr -d '\r\n' < "$artifacts/injector_IIS.release")"
if [ -z "$version" ]; then
  echo "empty version in $artifacts/injector_IIS.release" >&2
  exit 1
fi

mkdir -p sources/library
cp "$artifacts/injector_IIS.dll" sources/library/
echo "$version" > sources/version
find sources -type f
