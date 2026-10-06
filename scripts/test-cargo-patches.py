#!/usr/bin/env python3
"""Verify locked Cargo resolution uses the repository's patched dependencies."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parent.parent


class CargoPatchTests(unittest.TestCase):
    def test_locked_resolution_preserves_patches_and_caller_directory(self):
        rustc = subprocess.run(
            ["rustc", "-vV"], check=True, capture_output=True, text=True,
        )
        host = next(
            line.removeprefix("host: ")
            for line in rustc.stdout.splitlines() if line.startswith("host: ")
        )
        lockfile = REPO_ROOT / "Cargo.lock"
        original_lockfile = lockfile.read_bytes()
        with tempfile.TemporaryDirectory(prefix="peri-cargo-patches-") as temporary:
            for directory in (REPO_ROOT, Path(temporary)):
                with self.subTest(directory=directory):
                    result = subprocess.run(
                        [
                            str(REPO_ROOT / "scripts/cargo-rmcp-patched.sh"),
                            "metadata", "--locked", "--offline",
                            "--manifest-path", str(REPO_ROOT / "Cargo.toml"),
                            "--format-version", "1", "--filter-platform", host,
                        ],
                        cwd=directory, capture_output=True, text=True, timeout=120,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertNotIn("was not used in the crate graph", result.stderr)
                    packages = json.loads(result.stdout)["packages"]
                    for name, version in (
                        ("rmcp", "3.5.0"), ("hyper-util", "0.1.21"),
                        ("tokio", "1.53.1"), ("mio", "1.2.3"),
                    ):
                        matches = [package for package in packages if package["name"] == name]
                        self.assertEqual(len(matches), 1, name)
                        package = matches[0]
                        self.assertEqual(package["version"], version, name)
                        if name in ("rmcp", "hyper-util"):
                            self.assertIsNone(package["source"], name)
                            expected = REPO_ROOT / "target" / f"peri-{name}-patches"
                            self.assertTrue(
                                Path(package["manifest_path"]).is_relative_to(expected), name,
                            )
                        else:
                            self.assertTrue(package["source"].startswith(
                                f"git+https://github.com/guybedford/{name}?tag={version}-cf.emscripten#"
                            ), name)
                    self.assertEqual(lockfile.read_bytes(), original_lockfile)


if __name__ == "__main__":
    unittest.main()
