# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import zipfile

spec = importlib.util.spec_from_file_location("installer_release", Path(__file__).parents[1] / "installer-release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

SHA = "a" * 40


def trusted_run(**changes):
    run = {
        "id": 1,
        "status": "completed",
        "conclusion": "success",
        "path": ".github/workflows/installer.yml",
        "event": "push",
        "head_branch": "main",
        "head_sha": SHA,
        "repository": {"full_name": "DataDog/inject-browser-sdk"},
        "head_repository": {"full_name": "DataDog/inject-browser-sdk"},
    }
    run.update(changes)
    return run


class CheckReleaseRunTests(unittest.TestCase):
    def test_trusted_run_is_accepted(self):
        release.check_release_run(trusted_run(), SHA)

    def test_fork_pull_request_run_is_rejected(self):
        run = trusted_run(event="pull_request", head_branch="patch-1",
                          head_repository={"full_name": "someone/inject-browser-sdk"})
        with self.assertRaisesRegex(Exception, "event"):
            release.check_release_run(run, SHA)

    def test_trusted_run_for_another_sha_is_rejected(self):
        with self.assertRaisesRegex(Exception, "head_sha"):
            release.check_release_run(trusted_run(head_sha="b" * 40), SHA)

    def test_each_field_is_checked(self):
        for field, value in (
            ("status", "in_progress"),
            ("conclusion", "failure"),
            ("path", ".github/workflows/iis.yml"),
            ("event", "workflow_dispatch"),
            ("head_branch", "feature"),
            ("repository", {"full_name": "someone/inject-browser-sdk"}),
            ("head_repository", {"full_name": "someone/inject-browser-sdk"}),
        ):
            with self.subTest(field=field), self.assertRaisesRegex(Exception, field):
                release.check_release_run(trusted_run(**{field: value}), SHA)

    def test_sha_argument_must_be_full(self):
        release.release_sha(SHA)
        for value in ("abc123", "A" * 40, SHA + "0"):
            with self.subTest(value=value), self.assertRaises(Exception):
                release.release_sha(value)


class DownloadArtifactTests(unittest.TestCase):
    def setUp(self):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr("proxy-configurator", b"binary")
        self.zip_bytes = buffer.getvalue()
        self.digest = f"sha256:{hashlib.sha256(self.zip_bytes).hexdigest()}"

    def download(self, digest):
        artifact = {"id": 42, "name": "proxy-configurator-amd64", "digest": digest}
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(release, "gh_request", return_value=io.BytesIO(self.zip_bytes)) as request:
            files = release.download_artifact(artifact, Path(tmp))
            request.assert_called_once_with("/repos/DataDog/inject-browser-sdk/actions/artifacts/42/zip")
            return [f.name for f in files]

    def test_matching_digest_is_extracted(self):
        self.assertEqual(["proxy-configurator"], self.download(self.digest))

    def test_digest_mismatch_is_rejected(self):
        with self.assertRaisesRegex(Exception, "digest mismatch"):
            self.download("sha256:" + "0" * 64)

    def test_missing_digest_is_rejected(self):
        with self.assertRaisesRegex(Exception, "digest mismatch"):
            self.download(None)


if __name__ == "__main__":
    unittest.main()
