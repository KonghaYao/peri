#!/usr/bin/env python3
"""校验 learn-from-history 分析单元是否完整覆盖 snapshot manifest。"""

import argparse
import json
import re
import shutil
import sys
from pathlib import Path

from extract_daily import redact_sensitive
from run_history import DEFAULT_RUN_ROOT, sha256_file, write_private_json


def is_nonnegative_int(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def has_group_or_world_permissions(path):
    return bool(path.stat().st_mode & 0o077)


def resolve_run_path(run_dir, relative_path):
    """解析 manifest 相对路径，并拒绝绝对路径、run root 本身和目录逃逸。"""
    if not isinstance(relative_path, str) or not relative_path:
        raise ValueError("path must be a non-empty string")
    relative = Path(relative_path)
    if relative.is_absolute():
        raise ValueError("absolute path is not allowed")
    root = run_dir.resolve()
    resolved = (root / relative).resolve()
    if resolved == root:
        raise ValueError("run root is not a file path")
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise ValueError("path escapes run directory") from error
    return resolved


def contains_sensitive_credential(text):
    """复用提取器的保守规则检查 summary/sidecar 是否仍含凭据形态。"""
    return redact_sensitive(text) != text


def validate_unit(run_dir, unit):
    errors = []
    if not isinstance(unit, dict):
        return ["unit must be an object"]

    try:
        summary_path = resolve_run_path(run_dir, unit.get("summary_path"))
    except ValueError as error:
        errors.append(f"invalid summary_path: {error}")
        summary_path = None
    try:
        sidecar_path = resolve_run_path(run_dir, unit.get("sidecar_path"))
    except ValueError as error:
        errors.append(f"invalid sidecar_path: {error}")
        sidecar_path = None

    input_items = unit.get("inputs")
    if not isinstance(input_items, list):
        errors.append("unit inputs must be a list")
        input_items = []
    expected = {}
    for item in input_items:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            errors.append("invalid manifest input entry")
            continue
        path = item["path"]
        try:
            resolve_run_path(run_dir, path)
        except ValueError as error:
            errors.append(f"invalid manifest input path: {path}: {error}")
            continue
        if path in expected:
            errors.append(f"duplicate manifest input: {path}")
        expected[path] = item

    if not is_nonnegative_int(unit.get("expected_thread_count")) or unit.get("expected_thread_count") != len(input_items):
        errors.append("manifest expected_thread_count mismatch")
    message_counts = [item.get("message_count") for item in input_items if isinstance(item, dict)]
    if not all(is_nonnegative_int(value) for value in message_counts):
        errors.append("manifest input message_count must be a non-negative integer")
        expected_messages = None
    else:
        expected_messages = sum(message_counts)
    if expected_messages is None or unit.get("expected_message_count") != expected_messages:
        errors.append("manifest expected_message_count mismatch")

    if summary_path is None or not summary_path.is_file() or summary_path.stat().st_size == 0:
        errors.append("summary missing or empty")
    else:
        if has_group_or_world_permissions(summary_path):
            errors.append("summary permissions are not private")
        summary_text = summary_path.read_text(encoding="utf-8")
        if summary_text.strip().lower() in {"null", "none"}:
            errors.append("summary is null-like")
        if contains_sensitive_credential(summary_text):
            errors.append("summary contains sensitive credential pattern")

    if sidecar_path is None or not sidecar_path.is_file():
        errors.append("sidecar missing")
        return errors
    if has_group_or_world_permissions(sidecar_path):
        errors.append("sidecar permissions are not private")
    try:
        sidecar_text = sidecar_path.read_text(encoding="utf-8")
        sidecar = json.loads(sidecar_text)
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"sidecar unreadable: {type(error).__name__}")
        return errors
    if not isinstance(sidecar, dict):
        errors.append("sidecar must be an object")
        return errors
    if contains_sensitive_credential(sidecar_text):
        errors.append("sidecar contains sensitive credential pattern")

    if sidecar.get("unit_id") != unit.get("id"):
        errors.append("unit_id mismatch")
    if sidecar.get("status") != unit.get("expected_status") or unit.get("expected_status") != "analyzed":
        errors.append("unit status is not analyzed")
    if sidecar.get("thread_count") != unit.get("expected_thread_count"):
        errors.append("thread_count mismatch")
    if sidecar.get("message_count") != unit.get("expected_message_count"):
        errors.append("message_count mismatch")

    actual_entries = sidecar.get("input_files")
    if not isinstance(actual_entries, list):
        errors.append("input_files must be a list")
        return errors
    actual = {}
    for entry in actual_entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("path"), str):
            errors.append("invalid input_files entry")
            continue
        path = entry["path"]
        if path in actual:
            errors.append(f"duplicate sidecar input: {path}")
        actual[path] = entry
    if set(actual) != set(expected):
        missing = sorted(set(expected) - set(actual))
        extra = sorted(set(actual) - set(expected))
        if missing:
            errors.append(f"missing inputs: {', '.join(missing)}")
        if extra:
            errors.append(f"unexpected inputs: {', '.join(extra)}")

    reviewed = sidecar.get("degraded_inputs_reviewed", [])
    if not isinstance(reviewed, list) or not all(isinstance(path, str) for path in reviewed):
        errors.append("degraded_inputs_reviewed must be a string list")
        reviewed = []
    reviewed_set = set(reviewed)
    if not reviewed_set.issubset(expected):
        errors.append("degraded_inputs_reviewed contains unexpected paths")
    blocked = sidecar.get("blocked", [])
    if not isinstance(blocked, list):
        errors.append("blocked must be a list")
    elif blocked:
        errors.append("unit reports blocked inputs")

    for path, item in expected.items():
        try:
            input_path = resolve_run_path(run_dir, path)
        except ValueError:
            continue
        if not input_path.is_file():
            errors.append(f"input missing: {path}")
            continue
        if has_group_or_world_permissions(input_path):
            errors.append(f"input permissions are not private: {path}")
        if sha256_file(input_path) != item.get("sha256"):
            errors.append(f"input digest changed: {path}")
        entry = actual.get(path)
        if not entry:
            continue
        if entry.get("sha256") != item.get("sha256"):
            errors.append(f"sidecar digest mismatch: {path}")
        if entry.get("status") != "analyzed":
            errors.append(f"input not analyzed: {path}")
        if (item.get("truncations", 0) or item.get("parse_failures", 0)) and path not in reviewed_set:
            errors.append(f"degraded input not reviewed: {path}")

    findings = sidecar.get("findings")
    if not isinstance(findings, list):
        errors.append("findings must be a list")
    else:
        required = {"id", "classification", "evidence", "counterevidence", "frequency", "impact", "confidence", "fact_source", "acceptance"}
        allowed_classifications = {"rule_gap", "active_issue_covered", "skill_gap", "execution_deviation", "external_blocker"}
        for index, finding in enumerate(findings):
            if not isinstance(finding, dict):
                errors.append(f"finding {index} must be an object")
                continue
            missing_fields = sorted(required - set(finding))
            if missing_fields:
                errors.append(f"finding {index} missing: {', '.join(missing_fields)}")
            if finding.get("classification") not in allowed_classifications:
                errors.append(f"finding {index} has invalid classification")
            if not isinstance(finding.get("evidence"), list) or not finding.get("evidence"):
                errors.append(f"finding {index} needs evidence")
            if not isinstance(finding.get("counterevidence"), list):
                errors.append(f"finding {index} counterevidence must be a list")
            if not re.search(r"\d+\s*/\s*\d+", str(finding.get("frequency", ""))):
                errors.append(f"finding {index} frequency needs a denominator")
            if finding.get("impact") not in {"high", "medium", "low"}:
                errors.append(f"finding {index} has invalid impact")
            if finding.get("confidence") not in {"high", "medium", "low"}:
                errors.append(f"finding {index} has invalid confidence")
            if not str(finding.get("fact_source", "")).strip():
                errors.append(f"finding {index} needs fact_source")
            if not str(finding.get("acceptance", "")).strip():
                errors.append(f"finding {index} needs acceptance")
    return errors


def validate_run(run_dir):
    run_dir = Path(run_dir).expanduser().resolve()
    manifest_path = run_dir / "manifest.json"
    if not manifest_path.is_file():
        return {"status": "failed", "errors": ["manifest missing"], "units": []}
    try:
        manifest_text = manifest_path.read_text(encoding="utf-8")
        manifest = json.loads(manifest_text)
    except (OSError, json.JSONDecodeError) as error:
        return {"status": "failed", "errors": [f"manifest unreadable: {type(error).__name__}"], "units": []}

    errors = []
    if has_group_or_world_permissions(run_dir):
        errors.append("run directory permissions are not private")
    if has_group_or_world_permissions(manifest_path):
        errors.append("manifest permissions are not private")
    if contains_sensitive_credential(manifest_text):
        errors.append("manifest contains sensitive credential pattern")
    if manifest.get("version") != 1:
        errors.append("unsupported manifest version")
    if manifest.get("run_dir") != str(run_dir):
        errors.append("run_dir mismatch")
    if manifest.get("status") not in {"ready", "empty", "failed"}:
        errors.append("invalid manifest status")
    if manifest.get("failures"):
        errors.append("manifest contains extraction failures")

    snapshot = manifest.get("snapshot")
    if not isinstance(snapshot, dict):
        errors.append("snapshot metadata missing")
    else:
        try:
            snapshot_path = resolve_run_path(run_dir, snapshot.get("path"))
        except ValueError as error:
            errors.append(f"invalid snapshot path: {error}")
        else:
            if not snapshot_path.is_file():
                errors.append("snapshot missing")
            elif has_group_or_world_permissions(snapshot_path):
                errors.append("snapshot permissions are not private")
            elif sha256_file(snapshot_path) != snapshot.get("sha256"):
                errors.append("snapshot digest changed")

    days = manifest.get("days")
    if not isinstance(days, list):
        errors.append("days must be a list")
        days = []
    day_paths = set()
    thread_ids = set()
    day_totals = {"thread_count": 0, "message_count": 0, "truncations": 0, "parse_failures": 0}
    record_days = []
    for day_record in days:
        if not isinstance(day_record, dict):
            errors.append("day record must be an object")
            continue
        day = day_record.get("day")
        if not isinstance(day, str) or not day:
            errors.append("day record missing day")
        elif day in record_days:
            errors.append(f"duplicate day record: {day}")
        else:
            record_days.append(day)
        if day_record.get("status") != "passed":
            errors.append(f"day not passed: {day}")
        files = day_record.get("files")
        if not isinstance(files, list):
            errors.append(f"day files must be a list: {day}")
            continue
        if day_record.get("thread_count") != len(files):
            errors.append(f"day thread_count mismatch: {day}")
        day_totals["thread_count"] += len(files)
        for item in files:
            if not isinstance(item, dict) or not isinstance(item.get("path"), str):
                errors.append(f"invalid day file entry: {day}")
                continue
            path = item["path"]
            try:
                resolve_run_path(run_dir, path)
            except ValueError as error:
                errors.append(f"invalid day input path: {path}: {error}")
                continue
            if path in day_paths:
                errors.append(f"duplicate manifest input path: {path}")
            day_paths.add(path)
            thread_id = item.get("thread_id")
            if not isinstance(thread_id, str) or not thread_id:
                errors.append(f"thread_id missing: {path}")
            elif thread_id in thread_ids:
                errors.append(f"duplicate thread_id: {thread_id}")
            else:
                thread_ids.add(thread_id)
            for field in ("message_count", "truncations", "parse_failures"):
                if not is_nonnegative_int(item.get(field)):
                    errors.append(f"invalid {field}: {path}")
            if is_nonnegative_int(item.get("message_count")):
                day_totals["message_count"] += item["message_count"]
            if is_nonnegative_int(item.get("truncations")):
                day_totals["truncations"] += item["truncations"]
            if is_nonnegative_int(item.get("parse_failures")):
                day_totals["parse_failures"] += item["parse_failures"]

    window = manifest.get("window")
    active_days = window.get("active_days") if isinstance(window, dict) else None
    if not isinstance(active_days, list) or sorted(active_days) != sorted(record_days):
        errors.append("active_days mismatch")
    totals = manifest.get("totals")
    if not isinstance(totals, dict):
        errors.append("totals missing")
    else:
        for key, value in day_totals.items():
            if totals.get(key) != value:
                errors.append(f"totals {key} mismatch")

    units = manifest.get("units")
    if not isinstance(units, list):
        errors.append("units must be a list")
        units = []
    unit_reports = []
    seen_unit_ids = set()
    unit_paths = []
    for unit in units:
        unit_id = unit.get("id") if isinstance(unit, dict) else None
        if isinstance(unit, dict) and isinstance(unit.get("inputs"), list):
            unit_paths.extend(item.get("path") for item in unit["inputs"] if isinstance(item, dict))
        if not isinstance(unit_id, str) or not unit_id:
            unit_errors = ["unit id missing"]
        elif unit_id in seen_unit_ids:
            unit_errors = [f"duplicate unit id: {unit_id}"]
        else:
            seen_unit_ids.add(unit_id)
            unit_errors = validate_unit(run_dir, unit)
        unit_reports.append({"unit_id": unit_id, "status": "passed" if not unit_errors else "failed", "errors": unit_errors})
    if len(unit_paths) != len(set(unit_paths)):
        errors.append("input appears in multiple units")
    if set(unit_paths) != day_paths:
        errors.append("unit inputs do not match day inputs")
    if not units and manifest.get("status") != "empty":
        errors.append("non-empty run has no units")
    if units and manifest.get("status") != "ready":
        errors.append("run with units must have ready status")
    if any(report["status"] == "failed" for report in unit_reports):
        errors.append("one or more units failed validation")
    return {"status": "passed" if not errors else "failed", "errors": errors, "units": unit_reports}


def cleanup_inputs(run_dir, report):
    run_dir = Path(run_dir).expanduser().resolve()
    allowed_root = Path(DEFAULT_RUN_ROOT).resolve()
    try:
        relative = run_dir.relative_to(allowed_root)
    except ValueError as error:
        raise ValueError(f"cleanup is limited to {allowed_root}") from error
    if len(relative.parts) != 1 or relative.name in {"", ".", ".."}:
        raise ValueError("cleanup requires one concrete run directory")
    if report["status"] != "passed":
        raise ValueError("validation must pass before cleanup")
    for relative_path in ("snapshot", "extracted", "prompts"):
        path = resolve_run_path(run_dir, relative_path)
        if path.exists():
            shutil.rmtree(path)


def parse_args():
    parser = argparse.ArgumentParser(description="校验 learn-from-history run 的 summary sidecar")
    parser.add_argument("run_dir", help="run_history.py 输出的 run 目录")
    parser.add_argument("--cleanup-inputs", action="store_true", help="验证通过后删除 snapshot、提取物和 prompts")
    return parser.parse_args()


def main():
    args = parse_args()
    run_dir = Path(args.run_dir).expanduser().resolve()
    report = validate_run(run_dir)
    write_private_json(run_dir / "validation.json", report)
    for unit in report["units"]:
        print(f"{unit['unit_id']}: {unit['status']}")
        for error in unit["errors"]:
            print(f"  - {error}")
    for error in report["errors"]:
        print(f"ERROR: {error}", file=sys.stderr)
    if args.cleanup_inputs:
        try:
            cleanup_inputs(run_dir, report)
        except ValueError as error:
            print(f"ERROR: {error}", file=sys.stderr)
            return 1
        print("Sensitive run inputs removed after successful validation.")
    print(f"Validation: {report['status']}")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
