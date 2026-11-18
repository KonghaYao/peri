#!/usr/bin/env python3
"""Dependency guard regressions: rejected manifests need no Cargo or network."""
import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "tui_boundary", Path(__file__).with_name("check-tui-components.py")
)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class ComponentBoundaryTests(unittest.TestCase):
    def test_allowed_packages_can_be_renamed_or_inherited(self):
        manifest = {"dependencies": {
            "terminal": {"package": "ratatui", "version": "0.30.2"},
            "kit": {"workspace": True, "features": []},
        }}
        workspace = {"workspace": {"dependencies": {
            "kit": {"package": "ratatui-kit", "version": "0.12.0"},
        }}}
        self.assertEqual(guard.dependency_errors(manifest, workspace), [])

    def test_business_dependency_cannot_hide_in_any_dependency_table(self):
        for section in guard.DEPENDENCY_SECTIONS:
            for target in (None, "cfg(unix)", "wasm32-unknown-emscripten"):
                with self.subTest(section=section, target=target):
                    table = {section: {"backend": {
                        "package": "peri-resources", "version": "0.2.0",
                    }}}
                    manifest = table if target is None else {"target": {target: table}}
                    self.assertTrue(guard.dependency_errors(manifest, {}))

    def test_allowed_alias_cannot_hide_business_package(self):
        manifest = {"dependencies": {
            "ratatui": {"package": "peri-agent", "version": "0.2.0"},
        }}
        self.assertIn("'peri-agent'", guard.dependency_errors(manifest, {})[0])

    def test_workspace_inheritance_resolves_real_package_and_source(self):
        manifest = {"target": {"cfg(unix)": {"build-dependencies": {
            "ratatui": {"workspace": True},
        }}}}
        for dependency in (
            {"package": "peri-config", "version": "0.2.0"},
            {"path": "peri-resources"},
            {"git": "https://example.invalid/business.git"},
        ):
            with self.subTest(dependency=dependency):
                workspace = {"workspace": {"dependencies": {"ratatui": dependency}}}
                self.assertTrue(guard.dependency_errors(manifest, workspace))

    def test_local_source_cannot_masquerade_as_allowed_package(self):
        manifest = {"dependencies": {"ratatui": {"path": "../peri-resources"}}}
        self.assertTrue(guard.dependency_errors(manifest, {}))

    def test_missing_or_overridden_workspace_dependency_is_rejected(self):
        for declaration in ({"workspace": True}, {"workspace": True, "package": "peri-agent"}):
            with self.subTest(declaration=declaration):
                manifest = {"dependencies": {"ratatui": declaration}}
                workspace = {} if len(declaration) == 1 else {
                    "workspace": {"dependencies": {"ratatui": "0.30.2"}},
                }
                self.assertTrue(guard.dependency_errors(manifest, workspace))

    def test_static_lifetimes_are_not_global_state_declarations(self):
        for source in (
            "pub fn label() -> &'static str { \"ready\" }",
            "pub fn labels() -> &'static [&'static str] { &[] }",
        ):
            with self.subTest(source=source):
                self.assertIsNone(guard.GLOBAL_STATE.search(source))
        for source in ("static STATE: u8 = 0;", "static mut STATE: u8 = 0;"):
            with self.subTest(source=source):
                self.assertIsNotNone(guard.GLOBAL_STATE.search(source))

    def test_source_lint_and_dependency_check_run_together(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "Cargo.toml").write_text("[workspace]\n")
            component = root / "peri-tui-components"
            (component / "src").mkdir(parents=True)
            (component / "Cargo.toml").write_text('[dependencies]\nratatui = "0.30.2"\n')
            source = component / "src/lib.rs"
            source.write_text("pub fn harmless() {}\n")
            self.assertEqual(guard.check(root), [])
            source.write_text("use peri_agent::Agent;\nstatic STATE: u8 = 0;\n")
            self.assertEqual(len(guard.check(root)), 2)


if __name__ == "__main__":
    unittest.main()
