#!/usr/bin/env python3
"""Check direct component dependencies; source patterns are an additional lint.

This is not a Rust parser or a transitive dependency audit. Cargo compilation and
review still own macro expansion, included/generated code and dependency internals.
"""
from pathlib import Path
import re
import sys

if sys.version_info < (3, 11):
    raise SystemExit("TUI component boundary check requires Python 3.11+ (tomllib); select that python3 on PATH.")

import tomllib


ALLOWED_PACKAGES = {"ratatui", "ratatui-kit", "unicode-width", "mermaid-text"}
DEPENDENCY_SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
BUSINESS_IMPORT = re.compile(r"\bperi_(?:agent|acp|resources|config|tui|middlewares)\b")
GLOBAL_STATE = re.compile(r"(?<!')\bstatic\s+(?:mut\s+)?[A-Za-z_]|\b(?:lazy_static|atom)!")


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
                    errors.append(f"{location}: path/git sources require boundary review")
    return errors


def check(root: Path) -> list[str]:
    component = root / "peri-tui-components"
    manifest = tomllib.loads((component / "Cargo.toml").read_text())
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    errors = dependency_errors(manifest, workspace)
    for path in sorted((component / "src").rglob("*.rs")):
        source = path.read_text()
        if BUSINESS_IMPORT.search(source):
            errors.append(f"{path.relative_to(root)}: application reference")
        if GLOBAL_STATE.search(source):
            errors.append(f"{path.relative_to(root)}: global state declaration")
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
