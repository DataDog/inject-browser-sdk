"""Exercise the actual Python, CMake and PowerShell build version resolvers."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("iis_version", ROOT / "tasks/utils.py")
version = importlib.util.module_from_spec(spec)
spec.loader.exec_module(version)


class VersionContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "cmake").mkdir()
        shutil.copy2(ROOT / "cmake/version.cmake", self.root / "cmake/version.cmake")
        self.file = self.root / "VERSION"
        self.file.write_text("1.3.0\n")
        self.cmake = self.root / "check.cmake"
        self.cmake.write_text(
            'include("${CMAKE_CURRENT_LIST_DIR}/cmake/version.cmake")\n'
            'message("RESULT=${IIS_INJECTOR_VERSION}|${IIS_INJECTOR_VERSION_PRE}")\n'
        )
        self.ps = self.root / "check.ps1"
        # Pass paths through the environment rather than interpolating shell code.
        self.ps.write_text(
            "$ErrorActionPreference = 'Stop'\n"
            ". $env:IIS_VERSION_HELPER\n"
            "Get-IISBuildVersion -VersionFile $env:IIS_VERSION_FILE | ConvertTo-Json -Compress\n"
        )
        self.env = os.environ.copy()
        for key in ("MAJ_VER", "MIN_VER", "BUILD_VER", "VERSION_PRE", "CI_PIPELINE_IID", "IIS_RELEASE_BUILD"):
            self.env.pop(key, None)
        self.env["IIS_VERSION_HELPER"] = str(ROOT / "scripts/Get-IISBuildVersion.ps1")
        self.env["IIS_VERSION_FILE"] = str(self.file)

    def check(self, iid, expected, release=False):
        env = self.env.copy()
        if iid is not None:
            env["CI_PIPELINE_IID"] = iid
        if release:
            env["IIS_RELEASE_BUILD"] = "1"
        # Boundary IIDs trigger the near-limit warning on purpose; keep it out of build logs.
        with patch.dict(os.environ, env, clear=True), contextlib.redirect_stderr(io.StringIO()):
            if expected is None:
                with self.assertRaises(ValueError):
                    version.build_version(self.file)
            else:
                self.assertEqual(expected, version.build_version(self.file))
                # Existing module-stamp and OCI helpers unpack exactly two
                # values and require a three-component numeric version.
                numeric, pre = version.parse_version(self.file)
                self.assertEqual((".".join(expected[:3]), expected[3]), (numeric, pre))
        cmake = subprocess.run(["cmake", "-P", str(self.cmake)], env=env, text=True, capture_output=True)
        if expected is None:
            self.assertNotEqual(0, cmake.returncode, cmake.stderr)
        else:
            self.assertEqual(0, cmake.returncode, cmake.stderr)
            self.assertIn(f"RESULT={'.'.join(expected[:3])}|{expected[3]}", cmake.stderr)
        powershell = shutil.which("pwsh") or shutil.which("powershell")
        if powershell:
            ps = subprocess.run([powershell, "-NoProfile", "-File", str(self.ps)], env=env, text=True, capture_output=True)
            if expected is None:
                self.assertNotEqual(0, ps.returncode, ps.stdout)
            else:
                self.assertEqual(0, ps.returncode, ps.stderr)
                result = json.loads(ps.stdout)
                self.assertEqual(expected, tuple(result[k] for k in ("MAJ_VER", "MIN_VER", "BUILD_VER", "VERSION_PRE", "RELEASE_VER")))

    def test_pipeline_iid_boundaries(self):
        for iid, normalized in [(None, "0"), ("", "0"), ("0", "0"), ("00123", "123"), ("123", "123"), ("65535", "65535")]:
            with self.subTest(iid=iid):
                self.check(iid, ("1", "3", normalized, "dev", "1.3.0"))

    def test_invalid_pipeline_iids(self):
        for iid in ["-1", "+1", " 1", "1 ", "1\n", "1.0", "abc", "١", "65536", "9" * 100]:
            with self.subTest(iid=iid):
                self.check(iid, None)

    def test_release_mode_only_clears_suffix(self):
        self.check("321", ("1", "3", "321", "", "1.3.0"), release=True)
        self.file.write_text("1.3.0-beta")
        self.check("321", ("1", "3", "321", "", "1.3.0"), release=True)

    def test_release_versions(self):
        for raw, expected in [
            ("255.255.0-rc", ("255", "255", "1", "rc", "255.255.0")),
            ("000.001.002", ("0", "1", "1", "dev", "0.1.2")),
            ("1.3.65535", ("1", "3", "1", "dev", "1.3.65535")),
            ("256.1.0", None),
            ("1.256.0", None),
            ("1.3.65536", None),
            # The pre-release-please MAJ.MIN format is no longer accepted.
            ("1.3", None),
            ("1.3-dev", None),
        ]:
            with self.subTest(version=raw):
                self.file.write_text(raw)
                self.check("1", expected)

    def test_display_version(self):
        with patch.dict(os.environ, dict(self.env, CI_PIPELINE_IID="42"), clear=True):
            self.assertEqual("1.3.0-dev.42", version.build_version(self.file).display)
        with patch.dict(os.environ, dict(self.env, CI_PIPELINE_IID="42", IIS_RELEASE_BUILD="1"), clear=True):
            self.assertEqual("1.3.0", version.build_version(self.file).display)

    def test_warns_near_build_limit(self):
        for iid, warned in [("60000", False), ("60001", True)]:
            stderr = io.StringIO()
            with self.subTest(iid=iid), patch.dict(os.environ, dict(self.env, CI_PIPELINE_IID=iid), clear=True), \
                    contextlib.redirect_stderr(stderr):
                version.build_version(self.file)
            self.assertEqual(warned, "close to the Windows Installer limit" in stderr.getvalue())

    def test_local_overrides_reach_cmake_without_stale_cache(self):
        for build in ["41", "42"]:
            env = dict(self.env, MAJ_VER="2", MIN_VER="4", BUILD_VER=build, VERSION_PRE="dev")
            result = subprocess.run(["cmake", "-P", str(self.cmake)], env=env, text=True, capture_output=True)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn(f"RESULT=2.4.{build}|dev", result.stderr)


if __name__ == "__main__":
    unittest.main()
