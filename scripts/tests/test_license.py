# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("license_inventory", Path(__file__).parents[1] / "license.py")
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)


def detected(license_expression="MIT", holder="Example contributors"):
    return {
        "license_detections": [{"license_expression_spdx": license_expression}],
        "copyrights": [{"copyright": holder}],
        "holders": [{"holder": holder}],
    }


class SourceAttributionTests(unittest.TestCase):
    def test_foreign_notice_in_unregistered_file_fails(self):
        for path in ("src/code.rs", "assets/data.bin", "docs/reference.txt", ".github/action.yml"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "unregistered source attribution"):
                inventory.source_records({f"source/{path}": detected()}, {"third_party": {}})

    def test_foreign_license_without_copyright_still_fails(self):
        with self.assertRaisesRegex(ValueError, "unregistered source attribution"):
            inventory.source_records({"source/NOTICE": detected(holder="")}, {"third_party": {}})

    def test_third_party_apache_copyright_is_not_treated_as_first_party(self):
        with self.assertRaisesRegex(ValueError, "unregistered source attribution"):
            inventory.source_records({"source/copied.rs": detected("Apache-2.0")}, {"third_party": {}})

    def test_first_party_apache_header_needs_no_component(self):
        self.assertEqual([], inventory.source_records(
            {"source/code.rs": detected("Apache-2.0", "Datadog, Inc.")}, {"third_party": {}}
        ))

    def test_registered_component_tracks_notice_changes(self):
        config = {"third_party": {"example/project": {"origin": "https://example.com/project", "files": ["vendor/NOTICE"]}}}
        original = inventory.source_records({"source/vendor/NOTICE": detected()}, config)
        changed = inventory.source_records({"source/vendor/NOTICE": detected("BSD-3-Clause", "New contributors")}, config)
        self.assertEqual("MIT", original[0]["License"])
        self.assertEqual("BSD-3-Clause", changed[0]["License"])
        self.assertEqual("New contributors", changed[0]["Copyright"])
        self.assertNotEqual(inventory.render(original), inventory.render(changed))

    def test_deleted_registered_file_requires_registry_update(self):
        config = {"third_party": {"example/project": {"origin": "https://example.com/project", "files": ["gone.h"]}}}
        with self.assertRaisesRegex(ValueError, "source file is missing"):
            inventory.source_records({}, config)


class RustAttributionTests(unittest.TestCase):
    def setUp(self):
        self.package = {"name": "example", "repository": "https://example.com/project", "license": "MIT", "authors": []}

    def test_license_and_copyright_changes_change_the_inventory(self):
        original = inventory.rust_records([self.package], {"rust/0/LICENSE": detected()})
        changed = inventory.rust_records([{**self.package, "license": "BSD-3-Clause"}], {"rust/0/LICENSE": detected(holder="New contributors")})
        self.assertNotEqual(inventory.render(original), inventory.render(changed))
        self.assertEqual("New contributors", changed[0]["Copyright"])

    def test_removed_dependency_disappears(self):
        self.assertEqual([], inventory.rust_records([], {}))

    def test_two_versions_with_different_licenses_are_preserved(self):
        rows = inventory.rust_records([self.package, {**self.package, "license": "Apache-2.0"}], {})
        self.assertEqual({"MIT", "Apache-2.0"}, {row["License"] for row in rows})

    def test_missing_license_fails_instead_of_inventing_one(self):
        with self.assertRaisesRegex(ValueError, "missing Cargo origin/license"):
            inventory.rust_records([{**self.package, "license": None}], {})


if __name__ == "__main__":
    unittest.main()
