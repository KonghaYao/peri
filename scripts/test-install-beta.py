#!/usr/bin/env python3
"""Exercise the beta installer offline with real archives and mocked downloads."""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().with_name("install-beta.sh")


class BetaInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="peri-beta-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.commands = self.root / "commands"
        self.assets = self.root / "assets"
        self.install_dir = self.root / "install with spaces"
        self.download_log = self.root / "downloads.txt"
        for directory in (self.commands, self.assets, self.install_dir):
            directory.mkdir()
        self.stable = self.install_dir / "peri"
        self.stable.write_text("stable binary must not change\n")
        self.beta = self.install_dir / "peri-beta"
        self.beta.write_text("previous beta must survive failures\n")
        self.write_command("uname", """#!/bin/sh
case "$1" in
    -s) printf '%s\\n' "$MOCK_SYSTEM" ;;
    -m) printf '%s\\n' "$MOCK_ARCHITECTURE" ;;
    *) exit 1 ;;
esac
""")
        self.write_command("curl", """#!/bin/sh
url=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        https://*) url="$1" ;;
        --output) shift; output="$1" ;;
    esac
    shift
done
printf '%s\\n' "$url" >> "$MOCK_DOWNLOAD_LOG"
case "$url" in
    https://github.com/konghayao/peri/releases/download/peri-beta/*) ;;
    *) exit 22 ;;
esac
case "$url" in
    *"$MOCK_DOWNLOAD_FAILURE") [ -z "$MOCK_DOWNLOAD_FAILURE" ] || exit 22 ;;
esac
/bin/cp "$MOCK_ASSET_DIR/${url##*/}" "$output"
""")
        self.environment = {
            **os.environ,
            "PATH": f"{self.commands}{os.pathsep}{os.environ['PATH']}",
            "HOME": str(self.root),
            "TMPDIR": str(self.root),
            "PERI_BETA_INSTALL_DIR": str(self.install_dir),
            "MOCK_ASSET_DIR": str(self.assets),
            "MOCK_DOWNLOAD_LOG": str(self.download_log),
            "MOCK_SYSTEM": "Linux",
            "MOCK_ARCHITECTURE": "x86_64",
            "MOCK_DOWNLOAD_FAILURE": "",
        }
        self.environment.pop("PERI_BETA_INSTALL_PLATFORM", None)

    def write_command(self, name, contents):
        path = self.commands / name
        path.write_text(contents)
        path.chmod(0o755)

    def make_archive(self, platform="linux-x86_64", name="peri-beta", exit_code=0,
                     symlink=False):
        archive = self.assets / f"peri-beta-{platform}.tar.gz"
        payload = f"#!/bin/sh\nprintf 'beta fixture\\n'\nexit {exit_code}\n".encode()
        with tarfile.open(archive, "w:gz") as package:
            member = tarfile.TarInfo(name)
            member.mode = 0o755
            if symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = str(self.stable)
                package.addfile(member)
            else:
                member.size = len(payload)
                package.addfile(member, io.BytesIO(payload))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_suffix(".gz.sha256").write_text(f"{digest}  {archive.name}\n")
        return archive, payload

    def run_installer(self):
        return subprocess.run(["/bin/bash", str(INSTALLER)], env=self.environment,
                              text=True, capture_output=True, timeout=30)

    def assert_unchanged(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.stable.read_text(), "stable binary must not change\n")
        self.assertEqual(self.beta.read_text(), "previous beta must survive failures\n")
        self.assertEqual(list(self.install_dir.glob(".peri-beta.*")), [])

    def test_all_native_platforms(self):
        for system, architecture, platform in [
            ("Darwin", "x86_64", "macos-x86_64"),
            ("Darwin", "arm64", "macos-aarch64"),
            ("Linux", "amd64", "linux-x86_64"),
            ("Linux", "aarch64", "linux-aarch64"),
        ]:
            with self.subTest(platform=platform):
                _, payload = self.make_archive(platform)
                self.environment.update(MOCK_SYSTEM=system, MOCK_ARCHITECTURE=architecture)
                result = self.run_installer()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.beta.read_bytes(), payload)
                self.assertTrue(os.access(self.beta, os.X_OK))
                self.assertEqual(self.stable.read_text(), "stable binary must not change\n")
                self.assertIn(f"peri-beta-{platform}.tar.gz", self.download_log.read_text())

    def test_default_directory(self):
        self.make_archive()
        self.environment.pop("PERI_BETA_INSTALL_DIR")
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / ".local/bin/peri-beta").is_file())

    def test_manual_platform(self):
        self.make_archive("macos-aarch64")
        self.environment["PERI_BETA_INSTALL_PLATFORM"] = "macos-aarch64"
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("peri-beta-macos-aarch64.tar.gz", self.download_log.read_text())

    def test_unsupported_platforms(self):
        for system, architecture, override in [
            ("Windows_NT", "AMD64", ""),
            ("Linux", "riscv64", ""),
            ("Linux", "x86_64", "windows-x86_64"),
            ("Linux", "x86_64", "../../escape"),
        ]:
            with self.subTest(system=system, architecture=architecture, override=override):
                self.environment.update(MOCK_SYSTEM=system, MOCK_ARCHITECTURE=architecture,
                                        PERI_BETA_INSTALL_PLATFORM=override)
                self.assert_unchanged(self.run_installer())
                self.assertFalse(self.download_log.exists())

    def test_download_failures(self):
        self.make_archive()
        for suffix in (".tar.gz", ".sha256"):
            with self.subTest(suffix=suffix):
                self.environment["MOCK_DOWNLOAD_FAILURE"] = suffix
                self.assert_unchanged(self.run_installer())

    def test_checksum_failures(self):
        archive, _ = self.make_archive()
        checksum = archive.with_suffix(".gz.sha256")
        for contents in ("invalid\n", f"{'0' * 64}  {archive.name}\n",
                         f"{'0' * 64}  another-archive.tar.gz\n"):
            with self.subTest(contents=contents):
                checksum.write_text(contents)
                self.assert_unchanged(self.run_installer())

    def test_old_binary_name_is_rejected(self):
        self.make_archive(name="peri")
        self.assert_unchanged(self.run_installer())

    def test_symlink_archive_is_rejected(self):
        self.make_archive(symlink=True)
        self.assert_unchanged(self.run_installer())

    def test_unrunnable_binary_preserves_existing_install(self):
        self.make_archive(exit_code=1)
        self.assert_unchanged(self.run_installer())

    def test_existing_beta_symlink_does_not_overwrite_stable(self):
        _, payload = self.make_archive()
        self.beta.unlink()
        self.beta.symlink_to(self.stable)
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.beta.is_symlink())
        self.assertEqual(self.beta.read_bytes(), payload)
        self.assertEqual(self.stable.read_text(), "stable binary must not change\n")

    def test_checksum_backends(self):
        for backend in ("sha256sum", "shasum"):
            with self.subTest(backend=backend):
                isolated = self.root / backend
                isolated.mkdir()
                for command in ("tar", "mktemp", "install", "chmod", "mkdir", "mv", "rm"):
                    (isolated / command).symlink_to(shutil.which(command))
                for command in ("curl", "uname"):
                    (isolated / command).symlink_to(self.commands / command)
                checksum_path = shutil.which(backend)
                if checksum_path is None:
                    self.skipTest(f"{backend} is not available")
                (isolated / backend).symlink_to(checksum_path)
                self.make_archive()
                self.environment["PATH"] = str(isolated)
                result = self.run_installer()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
