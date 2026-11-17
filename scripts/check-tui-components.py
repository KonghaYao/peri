#!/usr/bin/env python3
"""Keep the component crate free of application dependencies and global state."""
from pathlib import Path
import re
import tomllib

root = Path(__file__).resolve().parents[1]
manifest = tomllib.loads((root / 'peri-tui-components/Cargo.toml').read_text())
allowed = {'ratatui', 'ratatui-kit', 'unicode-width', 'unicode-segmentation'}
assert set(manifest['dependencies']) <= allowed, 'review new component dependencies'
for path in (root / 'peri-tui-components/src').rglob('*.rs'):
    source = path.read_text()
    assert not re.search(r'\bperi_(?:agent|acp|resources|config|tui|middlewares)\b', source), path
    assert not re.search(r'\bstatic\s+(?:mut\s+)?[A-Za-z_]|\b(?:lazy_static|atom)!', source), path
print('PASS: component dependency allowlist and no application imports/global state')
