#!/usr/bin/env python3
"""Validate the feature register and local Markdown destinations, without network.

The Markdown scanner supports fenced blocks, inline code, inline/reference links,
ATX headings and explicit HTML anchors. It is deliberately not a Markdown renderer.
Repository-wide findings are diagnostics, not proof that external links work.
"""
import argparse
import html
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit


def prose(text):
    lines, fence = [], None
    for line in text.splitlines():
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            token = marker[1]
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            lines.append("")
        else:
            lines.append("" if fence else line)
    return "\n".join(lines)


def links(text):
    text = re.sub(r"(`+).*?\1", "", prose(text))
    definitions = dict(re.findall(r"(?m)^\s*\[([^\]]+)\]:\s*<?([^\s>]+)>?", text))
    destinations = [m[1] for m in re.finditer(r"!?\[[^\]\n]*\]\(<?([^\s)>]+)>?(?:\s+\"[^\"]*\")?\)", text)]
    for label, ref in re.findall(r"(?<!!)\[([^\]\n]+)\]\[([^\]\n]*)\]", text):
        key = ref or label
        if key in definitions:
            destinations.append(definitions[key])
    return destinations


def visible(text):
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]*>", "", text)
    text = re.sub(r"[`*~]", "", text)
    text = re.sub(r"(?<!\w)_([^_]+)_(?!\w)", r"\1", text)
    return html.unescape(text.replace("\\|", "|").replace("|", "").strip())


def anchors(text):
    result, counts = set(), {}
    text = prose(text)
    for line in text.splitlines():
        match = re.match(r"^ {0,3}#{1,6}\s+(.+?)(?:\s+#+)?\s*$", line)
        if match:
            title = visible(match[1]).lower()
            slug = re.sub(r"[^\w\- ]", "", title).replace(" ", "-")
            count = counts.get(slug, 0)
            counts[slug] = count + 1
            result.add(slug + (f"-{count}" if count else ""))
    result.update(re.findall(r'<(?:a|[a-z]+)\b[^>]*\b(?:id|name)=["\']([^"\']+)["\']', text))
    return result


def destination_error(root, source, destination, require_local=False):
    parsed = urlsplit(destination)
    if parsed.scheme or parsed.netloc:
        return "authority must be a repository-local path" if require_local else None
    target = (root / unquote(parsed.path).lstrip("/")) if parsed.path.startswith("/") else (source.parent / unquote(parsed.path))
    if not parsed.path:
        target = source
    target = target.resolve()
    if not target.is_relative_to(root.resolve()):
        return "destination escapes repository"
    if not target.exists():
        return "missing path"
    if parsed.fragment and target.suffix.lower() == ".md":
        if unquote(parsed.fragment) not in anchors(target.read_text()):
            return "missing anchor"
    return None


def table_rows(path):
    rows, active = [], False
    for number, line in enumerate(prose(path.read_text()).splitlines(), 1):
        if not line.startswith("|"):
            active = False
            continue
        fields = [cell.strip().replace("\\|", "|") for cell in re.split(r"(?<!\\)\|", line.strip().strip("|"))]
        if fields and fields[0] == "ID":
            active = True
        elif active and not all(re.fullmatch(r"[: -]+", cell) for cell in fields):
            rows.append((number, fields, line))
    return rows


def validate(root, catalog=None, acceptance=None):
    catalog = catalog or root / "docs/reference/feature-catalog.md"
    acceptance = acceptance or root / "docs/verification/feature-acceptance.md"
    errors, ids = [], set()
    if not catalog.is_file() or not acceptance.is_file():
        return ["feature catalog or acceptance map missing"]
    rows = table_rows(catalog)
    if not rows:
        errors.append("feature catalog has no recognized feature rows")
    mapped = {}
    for number, fields, _ in table_rows(acceptance):
        key = fields[0].strip("`")
        if key in mapped:
            errors.append(f"acceptance:{number}: duplicate ID {key}")
        mapped[key] = fields
    for number, fields, line in rows:
        key = fields[0].strip("`")
        prefix = f"catalog:{number}:{key}"
        if not re.fullmatch(r"F-[A-Z]+-[0-9]{3}", key):
            errors.append(f"{prefix}: invalid feature ID")
        if key in ids:
            errors.append(f"{prefix}: duplicate ID")
        ids.add(key)
        if len(fields) != 4:
            errors.append(f"{prefix}: expected ID, level, boundary, authority columns")
            continue
        level = fields[1].strip("`")
        if level not in {"L1", "L2", "L3", "L4", "L5", "U"}:
            errors.append(f"{prefix}: invalid level {level}")
        if len(visible(line)) > 100:
            errors.append(f"{prefix}: visible row exceeds 100 characters ({len(visible(line))})")
        if not visible(fields[2]):
            errors.append(f"{prefix}: empty feature boundary")
        sources = links(fields[3])
        if not sources:
            errors.append(f"{prefix}: no authority link")
        for link in sources:
            target = (catalog.parent / unquote(urlsplit(link).path)).resolve()
            if target.suffix != ".md" or target in {catalog.resolve(), acceptance.resolve()} or not target.is_file():
                errors.append(f"{prefix}: authority must be a Markdown contract, not a register or directory")
            error = destination_error(root, catalog, link, require_local=True)
            if error:
                errors.append(f"{prefix}: {error}: {link}")
        if level in {"L1", "L2", "L3", "L4"}:
            evidence = mapped.get(key)
            if not evidence:
                errors.append(f"{prefix}: missing acceptance mapping")
            elif len(evidence) != 5 or not evidence[3] or (not links(evidence[3]) and not re.search(r"缺口[：:]\s*\S+", evidence[3])):
                errors.append(f"{prefix}: acceptance needs evidence link or explicit gap")
        if level == "U":
            evidence = mapped.get(key, [])
            if len(evidence) != 5 or not re.search(r"缺口[：:]\s*[^|]*(?:评级|分级)", evidence[3]):
                errors.append(f"{prefix}: U requires explicit rating gap")
        if key in mapped:
            evidence = mapped[key]
            if len(evidence) != 5 or not visible(evidence[1]) or not visible(evidence[4]):
                errors.append(f"{prefix}: implementation and verification status must be nonempty")
            canonical = lambda source, link: str((source.parent / unquote(urlsplit(link).path)).resolve()) + "#" + unquote(urlsplit(link).fragment)
            actual = {canonical(acceptance, link) for link in links(evidence[2])} if len(evidence) >= 3 else set()
            expected = {canonical(catalog, link) for link in sources}
            if not actual or actual != expected:
                errors.append(f"{prefix}: acceptance contract differs from catalog authority")
    for key in mapped.keys() - ids:
        errors.append(f"acceptance: unknown feature ID {key}")
    for path in (catalog, acceptance):
        for link in links(path.read_text()):
            error = destination_error(root, path, link)
            if error:
                errors.append(f"{path.relative_to(root)}: {error}: {link}")
    return sorted(set(errors))


def scan(root):
    tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "*.md"], cwd=root).decode().split("\0")
    findings = []
    for relative in tracked:
        source = root / relative
        if not relative or not source.is_file():
            continue
        for link in links(source.read_text()):
            error = destination_error(root, source, link)
            if error:
                findings.append({"source": relative, "destination": link, "error": error})
    return sorted(findings, key=lambda item: (item["source"], item["destination"]))


def inventory_errors(root, inventory, expected):
    errors, seen = [], set()
    for line in prose(inventory.read_text()).splitlines():
        if not line.startswith("| ["):
            continue
        fields = [cell.strip() for cell in line.strip("|").split("|")]
        targets = links(fields[0])
        if len(targets) != 1 or len(fields) != 3:
            errors.append("inventory: malformed row")
            continue
        path = os.path.relpath(os.path.normpath(inventory.parent / unquote(urlsplit(targets[0]).path)), root)
        if path in seen:
            errors.append(f"inventory: duplicate path {path}")
        seen.add(path)
        if fields[1] not in {"保留", "改写", "合并", "归档", "删除"} or not fields[2]:
            errors.append(f"inventory: missing disposition/reason {path}")
        if fields[1] != "删除":
            error = destination_error(root, inventory, targets[0], require_local=True)
            if error:
                errors.append(f"inventory: {error}: {path}")
    errors.extend(f"inventory: missing baseline document {path}" for path in sorted(set(expected) - seen))
    return errors


def check_inventory(root):
    inventory = root / "docs/verification/authority-inventory-446.md"
    if not inventory.is_file():
        return ["inventory missing"]
    match = re.search(r"基线：`([0-9a-f]{40})`", inventory.read_text())
    if not match:
        return ["inventory baseline SHA missing"]
    try:
        tracked = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", match[1]], cwd=root, stderr=subprocess.PIPE).decode().splitlines()
    except subprocess.CalledProcessError:
        return ["inventory baseline unavailable; fetch its commit history before checking"]
    expected = [p for p in tracked if p.endswith(".md") and ("/" not in p or p.startswith(("docs/", "npm-packages/")) or (p.endswith("/README.md") and p.count("/") <= 2))]
    return inventory_errors(root, inventory, expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--inventory", action="store_true", help="check baseline authority inventory coverage")
    parser.add_argument("--scan-links", action="store_true", help="report all tracked Markdown link findings as JSON; exit nonzero on findings")
    args = parser.parse_args()
    result = scan(args.root) if args.scan_links else (check_inventory(args.root) if args.inventory else validate(args.root))
    if args.scan_links:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    elif result:
        print("\n".join(result))
    else:
        print("Documentation: PASS (structure and destinations; not runtime acceptance)")
    return bool(result)


if __name__ == "__main__":
    sys.exit(main())
