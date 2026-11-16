"""Validate the archive handed to the auto-inject consumer."""

import hashlib
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts" / "auto"))
from package_bundle import build_bundle  # noqa: E402


class BundleTest(unittest.TestCase):
    def test_contract_layout_and_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dll = root / "bootstrap.dll"
            pe = bytearray(128)
            pe[:2] = b"MZ"
            struct.pack_into("<I", pe, 0x3C, 64)
            pe[64:68] = b"PE\0\0"
            struct.pack_into("<H", pe, 68, 0x8664)
            struct.pack_into("<H", pe, 86, 0x2000)
            dll.write_bytes(pe)

            archive, digest = build_bundle(dll, root, "1.2.1-dev")
            self.assertEqual(archive.name, "iis-rum-bootstrap-1.2.1-dev-x64.zip")
            self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)
            self.assertEqual(
                archive.with_suffix(".zip.sha256").read_text(),
                f"{digest}  {archive.name}\n",
            )
            with zipfile.ZipFile(archive) as bundle:
                self.assertEqual(
                    set(bundle.namelist()),
                    {
                        "manifest.json",
                        "Invoke-IisRumLifecycle.ps1",
                        "Install-IisRum.ps1",
                        "ddiisrum-bootstrap.dll",
                        "RUM_schema.xml",
                    },
                )
                self.assertEqual(
                    json.loads(bundle.read("manifest.json")),
                    {
                        "contract_version": 1,
                        "architecture": "x64",
                        "runtime_abi": 1,
                        "bundle_version": "1.2.1-dev",
                    },
                )
                self.assertEqual(bundle.read("ddiisrum-bootstrap.dll"), pe)
            _, repeat_digest = build_bundle(dll, root, "1.2.1-dev")
            self.assertEqual(repeat_digest, digest)

            pe[68:70] = struct.pack("<H", 0x14C)
            dll.write_bytes(pe)
            with self.assertRaisesRegex(ValueError, "x64"):
                build_bundle(dll, root, "1.2.1-dev")
            with self.assertRaisesRegex(ValueError, "version"):
                build_bundle(dll, root, "../escape")


if __name__ == "__main__":
    unittest.main()
