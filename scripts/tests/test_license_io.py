# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

import contextlib
import importlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

from test_license import inventory


class ScanCompletenessTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.root = self.directory / "repo"
        self.root.mkdir()
        (self.root / ".gitignore").write_text("build/\n")
        crate = self.directory / "crate"
        crate.mkdir()
        for name in ("LICENSE", "README", "legal.txt"):
            (crate / name).write_text("notice\n")
        self.packages = [{"manifest_path": str(crate / "Cargo.toml"), "license_file": "legal.txt"}]
        self.stage = self.directory / "stage"
        self.report = self.directory / "report.json"

    def run_scan(self, omit=None, errors=None):
        def fake_scancode(*args, **kwargs):
            files = []
            for path in self.stage.rglob("*"):
                name = path.relative_to(self.stage).as_posix()
                if path.is_file() and name != omit:
                    files.append({"path": name, "type": "file", "scan_errors": errors or []})
            self.report.write_text(json.dumps({"files": files}))

        with mock.patch.object(inventory.subprocess, "run", side_effect=fake_scancode):
            return inventory.scan(self.root, self.packages, [".gitignore"], self.stage, self.report)

    def test_missing_rust_notice_fails_before_author_fallback(self):
        with self.assertRaisesRegex(ValueError, "rust/0/0-LICENSE"):
            self.run_scan(omit="rust/0/0-LICENSE")

    def test_missing_explicit_license_file_fails(self):
        with self.assertRaisesRegex(ValueError, "rust/0/2-legal.txt"):
            self.run_scan(omit="rust/0/2-legal.txt")

    def test_missing_repository_file_still_fails(self):
        with self.assertRaisesRegex(ValueError, "source/.gitignore"):
            self.run_scan(omit="source/0")

    def test_complete_scan_preserves_source_names_and_rust_notices(self):
        self.assertEqual({"source/.gitignore", "rust/0/0-LICENSE", "rust/0/1-README", "rust/0/2-legal.txt"}, set(self.run_scan()))

    def test_reported_scan_errors_fail(self):
        with self.assertRaisesRegex(ValueError, "scan failed"):
            self.run_scan(errors=["timeout"])


class InventoryWriteTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.target = self.root / "LICENSE-3rdparty.csv"
        self.original = "original inventory\n"
        self.target.write_text(self.original)
        self.target.chmod(0o644)

    def test_flush_failure_keeps_original_and_removes_temporary_file(self):
        with mock.patch.object(inventory.os, "fsync", side_effect=OSError("disk error")):
            with self.assertRaises(OSError):
                inventory.write_inventory(self.target, "replacement\n")
        self.assertEqual(self.original, self.target.read_text())
        self.assertEqual([self.target], list(self.root.iterdir()))

    def test_replace_failure_keeps_original_and_removes_temporary_file(self):
        with mock.patch.object(inventory.os, "replace", side_effect=OSError("replace error")):
            with self.assertRaises(OSError):
                inventory.write_inventory(self.target, "replacement\n")
        self.assertEqual(self.original, self.target.read_text())
        self.assertEqual([self.target], list(self.root.iterdir()))

    def test_successful_update_preserves_permissions(self):
        inventory.write_inventory(self.target, "replacement\n")
        self.assertEqual("replacement\n", self.target.read_text())
        self.assertEqual(0o644, self.target.stat().st_mode & 0o777)
        self.assertEqual([self.target], list(self.root.iterdir()))

    def test_legacy_cli_emits_only_csv_and_does_not_update_the_file(self):
        expected = inventory.render([])
        output, warnings = io.StringIO(), io.StringIO()
        with mock.patch.object(inventory, "ROOT", self.root), \
                mock.patch.object(inventory, "generate", return_value=expected), \
                mock.patch.object(sys, "argv", ["license.py", "--licenses-script", "old-tool.py"]), \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(warnings):
            self.assertEqual(0, inventory.main())
        self.assertEqual(expected, output.getvalue())
        self.assertIn("deprecated", warnings.getvalue())
        self.assertEqual(self.original, self.target.read_text())

    def test_check_mode_reports_drift_without_updating_file(self):
        with mock.patch.object(inventory, "ROOT", self.root), \
                mock.patch.object(inventory, "generate", return_value=inventory.render([])), \
                mock.patch.object(sys, "argv", ["license.py"]), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(1, inventory.main())
        self.assertEqual(self.original, self.target.read_text())

    def test_legacy_helper_imports_and_complete_attributions_still_work(self):
        with mock.patch.object(sys, "path", [str(inventory.ROOT / "scripts"), *sys.path]):
            legacy = importlib.import_module("license_tools")
        record = legacy.ThirdParty("component", "https://example.com", ["example"], ["contributors"])
        self.assertEqual(record, legacy.manual_mappings({"component": record})["component"])
        self.assertEqual({"component": record}, legacy.merge_dependencies({}, {"component": record}))


if __name__ == "__main__":
    unittest.main()
