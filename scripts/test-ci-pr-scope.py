"""Exercise PR test selection against real Git merge trees and Cargo manifests."""

from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("changed-crate-args.sh")


class PullRequestScope(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.git("init", "-q", "-b", "base")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts" / SCRIPT.name)
        rules = SCRIPT.with_name("ci-docs-only-patterns.txt")
        shutil.copyfile(rules, self.root / "scripts" / rules.name)
        self.write("Cargo.toml", '[workspace]\nmembers = ["ui", "peri-middlewares", "agent", "wasm"]\nresolver = "2"')
        for directory, name in (("ui", "peri-tui"), ("peri-middlewares", "common"),
                                ("agent", "peri-agent"), ("wasm", "peri-wasm")):
            manifest = f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n'
            if directory == "ui":
                manifest += "[target.'cfg(windows)'.dev-dependencies]\ncommon = { path = '../peri-middlewares' }"
            self.write(f"{directory}/Cargo.toml", manifest)
            self.write(f"{directory}/src/lib.rs", "// initial\n")
        self.git("add", ".")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD")
        self.git("switch", "-qc", "feature")

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def merge(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "feature", "--allow-empty")
        self.git("switch", "-q", "base")
        self.git("merge", "-q", "--no-ff", "feature", "-m", "PR merge")

    def scope(self, base=None):
        # A caller's local staged-path override must never bypass the PR diff.
        result = subprocess.check_output(
            ["bash", "scripts/" + SCRIPT.name, "--ci-pr", base or self.base],
            cwd=self.root, env={**os.environ, "CHANGED_DIFF_FILES": "ui/src/lib.rs"}, text=True,
        )
        return result.strip()

    def test_platform_dev_dependent_is_selected(self):
        self.write("peri-middlewares/src/lib.rs", "// changed\n")
        self.write("docs/change.md", "change")
        self.merge()
        self.assertEqual(self.scope(), "-p common -p peri-tui")

    def test_shared_fixture_outside_cargo_graph_requires_workspace(self):
        self.write("peri-middlewares/src/at_mention/work_fixture.rs", "// shared fixture\n")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")

    def test_package_readme_and_embedded_tui_locale_keep_the_package(self):
        self.write("ui/README.md", "crate documentation\n")
        self.write("ui/locales/zh-CN/main.ftl", "greeting = 你好\n")
        self.merge()
        self.assertEqual(self.scope(), "-p peri-tui")

    def test_rename_out_of_package_requires_workspace(self):
        self.git("mv", "ui/src/lib.rs", "unowned.rs")
        self.write("ui/src/lib.rs", "// replacement\n")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")

    def test_mixed_source_and_build_input_require_workspace(self):
        self.write("ui/src/lib.rs", "// changed\n")
        self.write("ui/build.rs", "fn main() {}")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")

    def test_agent_keeps_compact_gates(self):
        self.write("agent/src/lib.rs", "// changed\n")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")

    def test_unknown_native_test_coverage_requires_workspace(self):
        self.write("wasm/src/lib.rs", "// changed\n")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")

    def test_unreadable_cargo_graph_requires_workspace(self):
        self.write("ui/src/lib.rs", "// changed\n")
        self.merge()
        self.write("Cargo.toml", "[invalid manifest")
        self.assertEqual(self.scope(), "--workspace")

    def test_invalid_base_non_merge_and_empty_diff_require_workspace(self):
        self.assertEqual(self.scope(), "--workspace")
        self.merge()
        self.assertEqual(self.scope(), "--workspace")
        self.assertEqual(self.scope("0" * 40), "--workspace")


if __name__ == "__main__":
    unittest.main()
