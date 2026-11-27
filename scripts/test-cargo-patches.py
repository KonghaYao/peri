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
                    metadata = json.loads(result.stdout)
                    packages = metadata["packages"]
                    resolved_ids = {node["id"] for node in metadata["resolve"]["nodes"]}
                    matches = [
                        package for package in packages if package["name"] == "rmcp"
                    ]
                    self.assertEqual(len(matches), 1, "rmcp")
                    package = matches[0]
                    self.assertIn(package["id"], resolved_ids, "rmcp")
                    self.assertEqual(package["version"], "3.9.0", "rmcp")
                    self.assertIsNone(package["source"], "rmcp")
                    expected = REPO_ROOT / "target" / "peri-rmcp-patches"
                    self.assertTrue(
                        Path(package["manifest_path"]).is_relative_to(expected), "rmcp",
                    )
                    self.assertEqual(lockfile.read_bytes(), original_lockfile)


if __name__ == "__main__":
    unittest.main()
