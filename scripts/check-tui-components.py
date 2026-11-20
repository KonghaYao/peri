#!/usr/bin/env python3
"""Check direct component dependencies; source patterns are an additional lint.

This is not a Rust parser or a transitive dependency audit. Cargo compilation and
review still own macro expansion, included/generated code and dependency internals.

Boundary intent: `peri-tui-components` owns business-independent terminal UI
primitives *and their renderers* (Markdown, OpenUI fences, theme palettes). It may
depend on the presentation-only workspace crates `peri-genui` and `peri-theme`, but
never on application crates (`peri-tui`, `peri-agent`, `peri-acp`, ...).
"""
from pathlib import Path
import re
import sys

if sys.version_info < (3, 11):
    raise SystemExit("TUI component boundary check requires Python 3.11+ (tomllib); select that python3 on PATH.")

import tomllib


ALLOWED_PACKAGES = {
    "ratatui",
    "ratatui-kit",
    "ratatui-kit-markdown",
    # CommonMark fence scanning and Mermaid layout are presentation-only.
    "pulldown-cmark",
    "mermaid-text",
    "syntect",
    "tui-chart",
    "unicode-width",
    "unicode-segmentation",
    "parking_lot",
    "tracing",
    "serde_json",
    "peri-genui",
    "peri-theme",
    "serial_test",
}
# Workspace crates that are themselves business-independent presentation layers.
APPROVED_PATH_PACKAGES = {"peri-genui", "peri-theme"}
DEPENDENCY_SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
BUSINESS_IMPORT = re.compile(r"\bperi_(?:agent|acp|resources|config|tui|middlewares)\b")
MUTABLE_GLOBAL = re.compile(r"\bstatic\s+mut\b|\b(?:lazy_static|atom)!")
GLOBAL_STATE = re.compile(r"(?<!')\bstatic\s+(?:mut\s+)?[A-Za-z_]")
# Renderer-level caches/singletons with no application or session state. Each file
# is reviewed once here; `MUTABLE_GLOBAL` still rejects unsynchronized mutable state.
GLOBAL_STATE_ALLOWED = {
    "peri-tui-components/src/bin/storybook/charts/dashboard.rs":
        "immutable OnceLock for deterministic storybook bench fixtures (no session state)",
    "peri-tui-components/src/bin/storybook/stories/overlay.rs":
        "immutable OnceLock for deterministic storybook theme fixture (no session state)",
    "peri-tui-components/src/genui/library.rs":
        "immutable OnceLock for the static OpenUI schema",
    "peri-tui-components/src/markdown/boundary.rs":
        "thread-local scan counters (test instrumentation)",
    "peri-tui-components/src/markdown/code_block.rs":
        "syntect SyntaxSet/ThemeSet singletons and bounded highlight LRU cache",
    "peri-tui-components/src/markdown/metrics.rs":
        "thread-local parser counters; no cross-thread or session state",
    "peri-tui-components/src/markdown/parser.rs":
        "thread-local image scan counters (test instrumentation)",
}


def dependency_errors(manifest: dict, workspace: dict) -> list[str]:
    """Check every direct dependency regardless of target or feature activation."""
    errors = []
    inherited = workspace.get("workspace", {}).get("dependencies", {})
    tables = [("", manifest)]
    tables.extend((f"target.{target}.", table)
                  for target, table in manifest.get("target", {}).items())
    for prefix, table in tables:
        for section in DEPENDENCY_SECTIONS:
            for alias, declaration in table.get(section, {}).items():
                location = f"{prefix}{section}.{alias}"
                spec = declaration if isinstance(declaration, dict) else {}
                if spec.get("workspace") is True:
                    if alias not in inherited:
                        errors.append(f"{location}: missing workspace dependency")
                        continue
                    if set(spec) - {"workspace", "features", "optional", "default-features"}:
                        errors.append(f"{location}: unsupported workspace dependency override")
                        continue
                    declaration = inherited[alias]
                    spec = declaration if isinstance(declaration, dict) else {}
                package = spec.get("package", alias)
                if package not in ALLOWED_PACKAGES:
                    errors.append(f"{location}: package {package!r} is not approved")
                # A renamed local crate can otherwise masquerade as an allowed package.
                if "path" in spec or "git" in spec:
                    if package not in APPROVED_PATH_PACKAGES:
                        errors.append(f"{location}: path/git sources require boundary review")
                    elif spec.get("path") != f"../{package}":
                        errors.append(
                            f"{location}: approved workspace dependency must use ../{package}"
                        )
    return errors


def check(root: Path) -> list[str]:
    component = root / "peri-tui-components"
    manifest = tomllib.loads((component / "Cargo.toml").read_text())
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    errors = dependency_errors(manifest, workspace)
    for path in sorted((component / "src").rglob("*.rs")):
        source = path.read_text()
        relative = path.relative_to(root).as_posix()
        if BUSINESS_IMPORT.search(source):
            errors.append(f"{relative}: application reference")
        if MUTABLE_GLOBAL.search(source):
            errors.append(f"{relative}: mutable global state declaration")
        if GLOBAL_STATE.search(source) and relative not in GLOBAL_STATE_ALLOWED:
            errors.append(f"{relative}: global state declaration (not reviewed)")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    errors = check(root)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("PASS: component direct dependency boundaries and source lint")
    return 0


if __name__ == "__main__":
    sys.exit(main())
