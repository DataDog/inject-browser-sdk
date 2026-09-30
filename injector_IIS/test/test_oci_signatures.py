# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from invoke.exceptions import Exit
from injector_IIS.tasks.iis import OCI_PAYLOAD, OCI_SIGNATURE_MANIFEST, assert_signed, oci


class OciSignatureTests(unittest.TestCase):
    def test_default_packaging_rejects_missing_or_invalid_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for name, _ in OCI_PAYLOAD:
                (artifacts / name).write_bytes(b"unverified payload")
            manifest = artifacts / OCI_SIGNATURE_MANIFEST
            for content in (None, "not json", "[]", "{}"):
                if content is not None:
                    manifest.write_text(content)
                context = Mock()
                with self.subTest(manifest=content), self.assertRaises(Exit):
                    oci.body(
                        context,
                        artifacts_dir=str(artifacts),
                        out_dir=str(root / "out"),
                        version="1.2.3",
                    )
                context.run.assert_not_called()

    def test_default_packaging_checks_verified_payload_before_create(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            hashes = {}
            for name, _ in OCI_PAYLOAD:
                content = name.encode()
                (artifacts / name).write_bytes(content)
                hashes[name] = hashlib.sha256(content).hexdigest()
            (artifacts / OCI_SIGNATURE_MANIFEST).write_text(json.dumps(hashes))
            context = Mock()
            oci.body(
                context,
                artifacts_dir=str(artifacts),
                out_dir=str(root / "out"),
                version="1.2.3",
            )
            context.run.assert_called_once()
            (artifacts / "injector_IIS.dll").write_bytes(b"changed after verification")
            context = Mock()
            with self.assertRaises(Exit):
                oci.body(
                    context,
                    artifacts_dir=str(artifacts),
                    out_dir=str(root / "out"),
                    version="1.2.3",
                )
            context.run.assert_not_called()

    def test_signature_marker_without_verification_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            for name, content in (
                ("injector_IIS.dll", b"MZ\x2a\x86\x48\x86\xf7\x0d\x01\x07\x02"),
                ("RUMTools.ps1", b"# SIG # Begin signature block"),
            ):
                path = Path(directory) / name
                path.write_bytes(content)
                with self.assertRaises(Exit):
                    assert_signed(str(path), {})

    def test_change_after_windows_verification_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "injector_IIS.dll"
            original = b"verified payload"
            path.write_bytes(original)
            hashes = {path.name: hashlib.sha256(original).hexdigest()}
            assert_signed(str(path), hashes)
            path.write_bytes(original + b"modified after verification")
            with self.assertRaises(Exit):
                assert_signed(str(path), hashes)


if __name__ == "__main__":
    unittest.main()
