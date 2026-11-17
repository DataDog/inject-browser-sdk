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
version="$(tr -d '\r\n' < "$artifacts/injector_IIS.release")"
if [ -z "$version" ]; then
  echo "empty version in $artifacts/injector_IIS.release" >&2
  exit 1
fi
# Only signed iis-X.Y.Z tag builds may publish X.Y.Z. The release-please
# branch builds X.Y.Z unsigned, so it gets a dev version too.
if [[ $version != *-* && ! ${CI_COMMIT_TAG:-} =~ ^iis-[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  # MAJ.MIN.<parent pipeline IID>
  build="$(tr -d '\r\n' < "$artifacts/injector_IIS.version")"
  version="$version-dev.${build##*.}"
fi

mkdir -p sources/library sources/installer sources/modules/Datadog.RUM sources/schema
cp "$artifacts/injector_IIS.dll" sources/library/
cp "$artifacts/RUMTools.ps1" sources/installer/
# The folder name must match the module name for Import-Module.
cp "$artifacts/Datadog.RUM.psm1" "$artifacts/Datadog.RUM.psd1" sources/modules/Datadog.RUM/
cp "$artifacts/RUM_schema.xml" sources/schema/
echo "$version" > sources/version
find sources -type f
