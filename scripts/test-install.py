#!/usr/bin/env python3
"""Exercise the stable installer offline with real archives and mocked downloads.

覆盖发布契约：归档根目录内为 peri、逐资产 <asset>.sha256、旧布局与 checksums.txt 回退。
"""

import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().with_name("install.sh")
VERSION_TAG = "agent-v9.9.9"


class StableInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="peri-install-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.commands = self.root / "commands"
        self.assets = self.root / "assets"
        self.install_dir = self.root / "install with spaces"
        self.download_log = self.root / "downloads.txt"
        for directory in (self.commands, self.assets, self.install_dir):
            directory.mkdir()

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
        -o) shift; output="$1" ;;
        https://*) url="$1" ;;
    esac
    shift
done
printf '%s\\n' "$url" >> "$MOCK_DOWNLOAD_LOG"
case "$url" in
    https://api.github.com/*)
        /bin/cat "$MOCK_RELEASE_JSON"
        exit 0
        ;;
esac
case "$url" in
    https://github.com/*) ;;
    *) exit 22 ;;
esac
# 只有发布清单里列出的资产可下载；未发布返回 404 等价的退出码 22
grep -qxF "${url##*/}" "$MOCK_PUBLISHED" || exit 22
case "$url" in
    *"$MOCK_DOWNLOAD_FAILURE") [ -z "$MOCK_DOWNLOAD_FAILURE" ] || exit 22 ;;
esac
/bin/cp "$MOCK_ASSET_DIR/${url##*/}" "$output"
""")

        self.release_json = self.root / "release.json"
        self.published = self.root / "published.txt"
        self.published.write_text("")
        self.environment = {
            **os.environ,
            "PATH": f"{self.commands}{os.pathsep}{os.environ['PATH']}",
            "HOME": str(self.root),
            "TMPDIR": str(self.root),
            "PERI_INSTALL_DIR": str(self.install_dir),
            "PERI_INSTALL_VERSION": VERSION_TAG,
            "PERI_NO_PATH_HINT": "1",
            "MOCK_ASSET_DIR": str(self.assets),
            "MOCK_DOWNLOAD_LOG": str(self.download_log),
            "MOCK_RELEASE_JSON": str(self.release_json),
            "MOCK_PUBLISHED": str(self.published),
            "MOCK_SYSTEM": "Linux",
            "MOCK_ARCHITECTURE": "x86_64",
            "MOCK_DOWNLOAD_FAILURE": "",
        }
        self.environment.pop("PERI_INSTALL_PLATFORM", None)

    def write_command(self, name, contents):
        path = self.commands / name
        path.write_text(contents)
        path.chmod(0o755)

    def write_release(self, assets):
        """assets: [(资产名, 是否随发布附带)]；未附带的资产下载返回 404。"""
        entries = []
        published = []
        for name, is_published in assets:
            if not is_published:
                continue
            base = f"https://github.com/konghayao/peri/releases/download/{VERSION_TAG}/{name}"
            entries.append(
                '{"name":"%s","browser_download_url":"%s"}' % (name, base)
            )
            published.append(name)
        self.release_json.write_text(
            '{"tag_name":"%s","assets":[%s]}' % (VERSION_TAG, ",".join(entries))
        )
        self.published.write_text("".join(f"{name}\n" for name in published))

    def make_archive(self, platform="linux-x86_64", member="peri", exit_code=0):
        archive = self.assets / f"peri-{platform}.tar.gz"
        payload = f"#!/bin/sh\nprintf 'peri fixture\\n'\nexit {exit_code}\n".encode()
        with tarfile.open(archive, "w:gz") as package:
            info = tarfile.TarInfo(member)
            info.mode = 0o755
            info.size = len(payload)
            package.addfile(info, io.BytesIO(payload))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.assets / f"{archive.name}.sha256").write_text(
            f"{digest}  {archive.name}\n"
        )
        return archive, payload

    def run_installer(self):
        return subprocess.run(["/bin/bash", str(INSTALLER)], env=self.environment,
                              stdin=subprocess.DEVNULL, text=True,
                              capture_output=True, timeout=30)

    def installed_binary(self):
        return self.install_dir / VERSION_TAG / "peri"

    def assert_install_succeeded(self, result, payload):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        binary = self.installed_binary()
        self.assertEqual(binary.read_bytes(), payload)
        self.assertTrue(os.access(binary, os.X_OK))
        link = self.install_dir / "peri"
        self.assertTrue(link.is_symlink())
        # macOS 上 TMPDIR 是 /var → /private/var 的符号链接，两侧都取解析后的路径
        self.assertEqual(link.resolve(), binary.resolve())
        self.assertEqual(
            (self.install_dir / "current-version.txt").read_text().strip(),
            VERSION_TAG,
        )

    def assert_install_rejected(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.installed_binary().exists())
        self.assertFalse(list((self.install_dir / VERSION_TAG).glob("*.tar.gz")))

    def test_release_layout_installs(self):
        archive, payload = self.make_archive()
        self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
        result = self.run_installer()
        self.assert_install_succeeded(result, payload)
        self.assertIn(archive.name, self.download_log.read_text())
        self.assertIn(f"{archive.name}.sha256", self.download_log.read_text())

    def test_platform_matrix(self):
        for system, architecture, platform in [
            ("Darwin", "x86_64", "macos-x86_64"),
            ("Darwin", "arm64", "macos-aarch64"),
            ("Linux", "amd64", "linux-x86_64"),
            ("Linux", "aarch64", "linux-aarch64"),
            ("Linux", "riscv64", "linux-riscv64"),
        ]:
            with self.subTest(platform=platform):
                archive, payload = self.make_archive(platform)
                self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
                self.environment.update(MOCK_SYSTEM=system,
                                        MOCK_ARCHITECTURE=architecture)
                result = self.run_installer()
                self.assert_install_succeeded(result, payload)
                self.assertIn(archive.name, self.download_log.read_text())

    def test_latest_release_resolution(self):
        archive, payload = self.make_archive()
        self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
        self.environment.pop("PERI_INSTALL_VERSION")
        result = self.run_installer()
        self.assert_install_succeeded(result, payload)

    def test_checksum_entry_not_matching_asset_is_rejected(self):
        archive, _ = self.make_archive()
        self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
        checksum = self.assets / f"{archive.name}.sha256"
        for contents in ("invalid\n", f"{'0' * 64}  another-archive.tar.gz\n",
                         f"{'z' * 64}  {archive.name}\n"):
            with self.subTest(contents=contents):
                checksum.write_text(contents)
                self.assert_install_rejected(self.run_installer())

    def test_checksum_mismatch_is_rejected(self):
        archive, _ = self.make_archive()
        self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
        (self.assets / f"{archive.name}.sha256").write_text(
            f"{'0' * 64}  {archive.name}\n"
        )
        self.assert_install_rejected(self.run_installer())

    def test_checksums_txt_fallback(self):
        archive, payload = self.make_archive()
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.assets / "checksums.txt").write_text(
            f"{'1' * 64}  peri-macos-aarch64.tar.gz\n{digest}  {archive.name}\n"
        )
        self.write_release([(archive.name, True), ("checksums.txt", True)])
        result = self.run_installer()
        self.assert_install_succeeded(result, payload)
        self.assertIn("checksums.txt", self.download_log.read_text())

    def test_missing_checksum_is_rejected(self):
        archive, _ = self.make_archive()
        self.write_release([(archive.name, True)])
        self.assert_install_rejected(self.run_installer())

    def test_archive_download_failure(self):
        archive, _ = self.make_archive()
        self.write_release([(archive.name, True), (f"{archive.name}.sha256", True)])
        self.environment["MOCK_DOWNLOAD_FAILURE"] = archive.name
        self.assert_install_rejected(self.run_installer())

    def test_legacy_archive_layout_installs(self):
        # ≤ agent-v3.19.x 的发布：归档根目录内是 peri-<platform>，且只有 checksums.txt；
        # 回退分支待旧版本退场后删除。
        archive, payload = self.make_archive(member="peri-linux-x86_64")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.assets / "checksums.txt").write_text(f"{digest}  {archive.name}\n")
        self.write_release([(archive.name, True), ("checksums.txt", True)])
        result = self.run_installer()
        self.assert_install_succeeded(result, payload)

    def test_unsupported_platform(self):
        self.environment.update(MOCK_SYSTEM="Linux", MOCK_ARCHITECTURE="ppc64le")
        self.assert_install_rejected(self.run_installer())


if __name__ == "__main__":
    unittest.main()
