#!/usr/bin/env python3
"""校验 learn-from-history 分析单元是否完整覆盖 snapshot manifest。"""

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import sys
from pathlib import Path

from extract_daily import redact_sensitive
from run_history import DEFAULT_RUN_ROOT, MANIFEST_VERSION, sha256_file, write_private_json

ALLOWED_CLASSIFICATIONS = {
    "rule_gap", "active_issue_covered", "skill_gap", "execution_deviation", "external_blocker",
}
ALLOWED_SURFACES = {
    "standard", "module_guidance", "active_issue", "skill", "tool_description",
    "tool_implementation", "middleware", "subagent", "memory", "configuration",
    "implementation", "test", "external", "none",
}


def is_nonnegative_int(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def has_group_or_world_permissions(path):
    return bool(path.stat().st_mode & 0o077)


def open_directory_nofollow(path):
    """逐级打开绝对目录；任一组件是 symlink 时 fail closed。"""
    nofollow = getattr(os, "O_NOFOLLOW", None)
    if nofollow is None or os.open not in os.supports_dir_fd:
        raise OSError("secure directory-relative open is unavailable")
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("secure root must be absolute")
    flags = os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
    parts = path.parts
    current_fd = os.open(parts[0], flags)
    try:
        for part in parts[1:]:
            next_fd = os.open(part, flags, dir_fd=current_fd)
            os.close(current_fd)
            current_fd = next_fd
        if not stat.S_ISDIR(os.fstat(current_fd).st_mode):
            raise OSError("secure root is not a directory")
        return current_fd
    except Exception:
        os.close(current_fd)
        raise


def open_regular_file_at(root_fd, relative_path):
    """从固定目录 fd 逐级打开普通文件，不跟随任何 symlink。"""
    nofollow = getattr(os, "O_NOFOLLOW", None)
    if nofollow is None or os.open not in os.supports_dir_fd:
        raise OSError("secure directory-relative open is unavailable")
    if not isinstance(relative_path, str) or not relative_path:
        raise ValueError("path must be a non-empty string")
    relative = Path(relative_path)
    if relative.is_absolute() or any(part in {"", ".", ".."} for part in relative.parts):
        raise ValueError("path must stay beneath secure root")
    if not relative.parts:
        raise ValueError("path must identify a file")

    directory_flags = (
        os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
    )
    file_flags = (
        os.O_RDONLY | nofollow | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
    )
    directory_fd = os.dup(root_fd)
    try:
        for part in relative.parts[:-1]:
            next_fd = os.open(part, directory_flags, dir_fd=directory_fd)
            os.close(directory_fd)
            directory_fd = next_fd
        file_fd = os.open(relative.parts[-1], file_flags, dir_fd=directory_fd)
    finally:
        os.close(directory_fd)
    try:
        if not stat.S_ISREG(os.fstat(file_fd).st_mode):
            raise OSError("attestation artifact is not a regular file")
        return file_fd
    except Exception:
        os.close(file_fd)
        raise


def read_file_at(root_fd, relative_path):
    """由同一普通文件 fd 读取内容、权限与 digest。"""
    file_fd = open_regular_file_at(root_fd, relative_path)
    with os.fdopen(file_fd, "rb") as handle:
        file_stat = os.fstat(handle.fileno())
        content = handle.read()
    return content, file_stat, hashlib.sha256(content).hexdigest()


def read_json_file_at(root_fd, relative_path):
    """由同一 fd 读取并散列 JSON，避免内容与 digest 来自不同 inode。"""
    content, _, digest = read_file_at(root_fd, relative_path)
    text = content.decode("utf-8")
    return json.loads(text), digest


def resolve_run_path(run_dir, relative_path):
    """解析 manifest 相对路径，并拒绝绝对路径、run root 本身和目录逃逸。"""
    if not isinstance(relative_path, str) or not relative_path:
        raise ValueError("path must be a non-empty string")
    relative = Path(relative_path)
    if relative.is_absolute():
        raise ValueError("absolute path is not allowed")
    try:
        root = run_dir.resolve()
        resolved = (root / relative).resolve()
    except (OSError, RuntimeError) as error:
        raise ValueError("path cannot be resolved safely") from error
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


def has_evidence_locator(reference, expected_paths):
    """证据必须精确指向当前 unit 输入，并包含非空定位描述。"""
    if not isinstance(reference, str):
        return False
    path, separator, locator = reference.partition(" :: ")
    return separator == " :: " and path in expected_paths and bool(locator.strip())


def has_stable_id(value, prefix):
    return isinstance(value, str) and bool(
        re.fullmatch(rf"{re.escape(prefix)}-[A-Za-z0-9][A-Za-z0-9._-]*", value)
    )


def normalize_check(value):
    return " ".join(value.split()) if isinstance(value, str) else value


def finding_contract_snapshot(finding):
    return {
        key: finding.get(key)
        for key in (
            "id", "classification", "failure_pattern", "root_cause", "target_surface",
            "predicted_fixes", "risk_regressions", "acceptance",
        )
    }


def finding_contract_digest(finding):
    """为归因所需的 finding 核心契约生成稳定摘要。"""
    contract = finding_contract_snapshot(finding)
    encoded = json.dumps(contract, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def acceptance_contract(change, label):
    """验收必须明确区分目标修复与保留成功模式，且检查不可重复。"""
    acceptance = change.get("acceptance")
    if not isinstance(acceptance, dict):
        return [f"{label} acceptance must be an object"], []

    errors = []
    checks = []
    for kind in ("target", "preserved_success"):
        values = acceptance.get(kind)
        if not isinstance(values, list) or not values or not all(
            isinstance(item, str) and item.strip() for item in values
        ):
            errors.append(f"{label} acceptance needs non-empty {kind} checks")
            continue
        checks.extend(values)
    normalized_checks = [normalize_check(check) for check in checks]
    if len(normalized_checks) != len(set(normalized_checks)):
        errors.append(f"{label} acceptance checks must be unique")
    return errors, checks


def attribution_observation_errors(
    observations,
    field,
    historical_change,
    finding_contracts,
    source_run_id,
    source_manifest_sha256,
    label,
):
    """把当前 finding、旧预测/风险与观察结果绑定为一条可核查记录。"""
    errors = []
    if not isinstance(observations, list):
        return [f"{label} {field} must be a list"]
    prior_field = "predicted_fixes" if field == "observed_fixes" else "risk_regressions"
    prior_contracts = historical_change.get(prior_field, []) if isinstance(historical_change, dict) else []
    if not isinstance(prior_contracts, list):
        prior_contracts = []
    expected_classification = historical_change.get("classification") if isinstance(historical_change, dict) else None
    expected_surface = historical_change.get("target_surface") if isinstance(historical_change, dict) else None
    for index, observation in enumerate(observations):
        observation_label = f"{label} {field} {index}"
        if not isinstance(observation, dict):
            errors.append(f"{observation_label} must be an object")
            continue
        for key in (
            "source_finding", "source_run_id", "source_manifest_sha256", "prior_contract",
            "outcome", "observed_delta",
        ):
            if not isinstance(observation.get(key), str) or not observation[key].strip():
                errors.append(f"{observation_label} needs {key}")
        if observation.get("source_run_id") != source_run_id:
            errors.append(f"{observation_label} source_run_id mismatch")
        if observation.get("source_manifest_sha256") != source_manifest_sha256:
            errors.append(f"{observation_label} source_manifest_sha256 mismatch")
        if observation.get("outcome") not in {"fixed", "improved", "unchanged", "regressed", "not_observed"}:
            errors.append(f"{observation_label} has invalid outcome")
        finding = finding_contracts.get(observation.get("source_finding"))
        if finding is None:
            errors.append(f"{observation_label} references unknown finding")
        else:
            if finding.get("classification") != expected_classification:
                errors.append(f"{observation_label} classification differs from prior change")
            if finding.get("target_surface") != expected_surface:
                errors.append(f"{observation_label} target_surface differs from prior change")
            expected_snapshot = finding_contract_snapshot(finding)
            if observation.get("finding_contract") != expected_snapshot:
                errors.append(f"{observation_label} finding_contract mismatch")
            if observation.get("finding_digest") != finding_contract_digest(finding):
                errors.append(f"{observation_label} finding_digest mismatch")
        if observation.get("prior_contract") not in prior_contracts:
            errors.append(f"{observation_label} does not match prior {prior_field}")
    return errors


def attribution_verdict_errors(
    attribution,
    historical_change,
    finding_contracts,
    source_run_id,
    source_manifest_sha256,
    label,
):
    errors = []
    for field in ("observed_fixes", "observed_regressions"):
        errors.extend(attribution_observation_errors(
            attribution.get(field),
            field,
            historical_change,
            finding_contracts,
            source_run_id,
            source_manifest_sha256,
            label,
        ))
    fixes = attribution.get("observed_fixes")
    regressions = attribution.get("observed_regressions")
    fix_items = fixes if isinstance(fixes, list) else []
    regression_items = regressions if isinstance(regressions, list) else []
    fix_outcomes = {item.get("outcome") for item in fix_items if isinstance(item, dict)}
    regression_outcomes = {item.get("outcome") for item in regression_items if isinstance(item, dict)}
    verdict = attribution.get("verdict")
    if verdict == "keep":
        if not ({"fixed", "improved"} & fix_outcomes):
            errors.append(f"{label} keep needs fixed or improved outcome")
        if "regressed" in fix_outcomes | regression_outcomes:
            errors.append(f"{label} keep conflicts with regressed outcome")
    elif verdict == "revert":
        if "regressed" not in regression_outcomes:
            errors.append(f"{label} revert needs regressed outcome")
        if {"fixed", "improved"} & fix_outcomes:
            errors.append(f"{label} revert conflicts with fixed or improved outcome")
    elif verdict == "improve" and not ({"improved", "unchanged", "regressed"} & (fix_outcomes | regression_outcomes)):
        errors.append(f"{label} improve needs an actionable outcome")
    elif verdict == "inconclusive" and not (
        {"not_observed", "unchanged"} & (fix_outcomes | regression_outcomes) or not fixes and not regressions
    ):
        errors.append(f"{label} inconclusive conflicts with conclusive outcomes")
    return errors


def load_finding_contracts(run_dir, manifest, run_fd=None, expected_sidecars=None):
    contracts = {}
    units = manifest.get("units")
    if not isinstance(units, list):
        return None if expected_sidecars is not None else contracts

    expected = expected_sidecars if isinstance(expected_sidecars, list) else None
    actual_sidecars = []
    owns_fd = run_fd is None
    try:
        if owns_fd:
            resolved_run_dir = Path(run_dir).expanduser().resolve()
            run_fd = open_directory_nofollow(resolved_run_dir)
        for unit in units:
            if not isinstance(unit, dict):
                if expected is not None:
                    return None
                continue
            try:
                sidecar_path = unit.get("sidecar_path")
                sidecar, sidecar_sha256 = read_json_file_at(run_fd, sidecar_path)
            except (
                OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
                json.JSONDecodeError,
            ):
                if expected is not None:
                    return None
                continue
            actual_sidecars.append({
                "unit_id": unit.get("id"),
                "path": sidecar_path,
                "sha256": sidecar_sha256,
            })
            if not isinstance(sidecar, dict) or not isinstance(sidecar.get("findings"), list):
                if expected is not None:
                    return None
                continue
            for finding in sidecar["findings"]:
                if isinstance(finding, dict) and isinstance(finding.get("id"), str):
                    contracts[f"{unit.get('id')}/{finding['id']}"] = finding
        if expected is not None and actual_sidecars != expected:
            return None
        return contracts
    except (OSError, ValueError, RuntimeError, TypeError):
        return None if expected is not None else contracts
    finally:
        if owns_fd and run_fd is not None:
            os.close(run_fd)


def load_attested_historical_findings(repository_root, source, payload):
    """从 canonical run 中保留的 sidecar 与 validation attestation 恢复可信 finding。"""
    allowed_root_fd = None
    run_fd = None
    repository_fd = None
    try:
        if not isinstance(payload, dict):
            return None
        run_id = payload.get("run_id")
        if (
            not isinstance(run_id, str)
            or not run_id
            or Path(run_id).parts != (run_id,)
            or run_id in {".", ".."}
        ):
            return None
        allowed_root = Path(DEFAULT_RUN_ROOT).expanduser().resolve()
        expected_run_dir = allowed_root / run_id
        source_run_dir = payload.get("source_run_dir") or str(expected_run_dir)
        if not isinstance(source_run_dir, str) or source_run_dir != str(expected_run_dir):
            return None

        allowed_root_fd = open_directory_nofollow(allowed_root)
        directory_flags = (
            os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
        )
        run_fd = os.open(run_id, directory_flags, dir_fd=allowed_root_fd)
        if not stat.S_ISDIR(os.fstat(run_fd).st_mode):
            return None

        manifest, manifest_sha256 = read_json_file_at(run_fd, "manifest.json")
        validation, _ = read_json_file_at(run_fd, "validation.json")
        if not isinstance(manifest, dict) or not isinstance(validation, dict):
            return None
        if manifest.get("run_id") != run_id or manifest.get("run_dir") != str(expected_run_dir):
            return None
        if manifest_sha256 != payload.get("source_manifest_sha256"):
            return None
        validation_attestation = validation.get("attestation")
        source_sidecars = payload.get("source_sidecars")
        if (
            not isinstance(source_sidecars, list)
            or not isinstance(validation_attestation, dict)
            or validation_attestation.get("manifest_sha256") != manifest_sha256
            or validation_attestation.get("sidecars") != source_sidecars
        ):
            return None

        repository_root = Path(repository_root).expanduser().resolve()
        expected_decision_path = repository_root / source
        repository_fd = open_directory_nofollow(repository_root)
        decision, decision_sha256 = read_json_file_at(repository_fd, source)
        if decision != payload:
            return None
        decision_report = validation.get("decision_manifest")
        if (
            validation.get("status") != "passed"
            or not isinstance(decision_report, dict)
            or decision_report.get("status") != "passed"
            or decision_report.get("path") != str(expected_decision_path)
            or decision_report.get("sha256") != decision_sha256
        ):
            return None
        return load_finding_contracts(
            expected_run_dir,
            manifest,
            run_fd=run_fd,
            expected_sidecars=source_sidecars,
        )
    except (
        OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
        json.JSONDecodeError,
    ):
        return None
    finally:
        if repository_fd is not None:
            os.close(repository_fd)
        if run_fd is not None:
            os.close(run_fd)
        if allowed_root_fd is not None:
            os.close(allowed_root_fd)


def historical_observation_is_valid(
    item,
    prior_contracts,
    change,
    attribution_run_id,
    attribution_manifest_sha256,
    attested_findings,
):
    if not isinstance(item, dict) or item.get("prior_contract") not in prior_contracts:
        return False
    if (
        item.get("source_run_id") != attribution_run_id
        or item.get("source_manifest_sha256") != attribution_manifest_sha256
    ):
        return False
    if item.get("outcome") not in {"fixed", "improved", "unchanged", "regressed", "not_observed"}:
        return False
    if not all(
        isinstance(item.get(key), str) and item[key].strip()
        for key in ("source_finding", "observed_delta", "finding_digest")
    ):
        return False
    snapshot = item.get("finding_contract")
    attested_finding = attested_findings.get(item.get("source_finding"))
    return (
        isinstance(snapshot, dict)
        and isinstance(attested_finding, dict)
        and snapshot == finding_contract_snapshot(attested_finding)
        and snapshot.get("classification") == change.get("classification")
        and snapshot.get("target_surface") == change.get("target_surface")
        and item["finding_digest"] == finding_contract_digest(snapshot)
    )


def historical_terminal_verdict_is_valid(
    attribution,
    attribution_source,
    attribution_payload,
    prior_changes,
    attested_findings,
):
    """只有绑定更早实施项、携带不可变 finding 契约的旧终局 verdict 才能关闭归因。"""
    if not isinstance(attribution, dict) or attribution.get("verdict") not in {"keep", "revert"}:
        return False
    source = attribution.get("source")
    if not is_canonical_prior_source(source) or source >= attribution_source:
        return False
    key = (source, attribution.get("change_id"))
    change = prior_changes.get(key)
    if change is None or change.get("status") != "implemented":
        return False
    if not isinstance(attribution.get("rationale"), str) or not attribution["rationale"].strip():
        return False
    if not isinstance(attested_findings, dict):
        return False
    fix_contracts = change.get("predicted_fixes", [])
    risk_contracts = change.get("risk_regressions", [])
    if not isinstance(fix_contracts, list) or not isinstance(risk_contracts, list):
        return False
    fixes = attribution.get("observed_fixes")
    regressions = attribution.get("observed_regressions")
    if not isinstance(fixes, list) or not isinstance(regressions, list):
        return False
    if not all(
        historical_observation_is_valid(
            item,
            fix_contracts,
            change,
            attribution_payload.get("run_id"),
            attribution_payload.get("source_manifest_sha256"),
            attested_findings,
        )
        for item in fixes
    ):
        return False
    if not all(
        historical_observation_is_valid(
            item,
            risk_contracts,
            change,
            attribution_payload.get("run_id"),
            attribution_payload.get("source_manifest_sha256"),
            attested_findings,
        )
        for item in regressions
    ):
        return False
    fix_outcomes = {item["outcome"] for item in fixes}
    regression_outcomes = {item["outcome"] for item in regressions}
    if attribution["verdict"] == "keep":
        return bool({"fixed", "improved"} & fix_outcomes) and "regressed" not in fix_outcomes | regression_outcomes
    return "regressed" in regression_outcomes and not ({"fixed", "improved"} & fix_outcomes)


def verification_contract_errors(change, label):
    """实施项必须用唯一、结构化记录逐项证明 acceptance 已通过。"""
    errors = []
    verification = change.get("verification")
    if not isinstance(verification, list):
        return [f"{label} verification must be a list"]

    acceptance_errors, acceptance_checks = acceptance_contract(change, label)
    errors.extend(acceptance_errors)
    accepted = {normalize_check(check) for check in acceptance_checks}
    verification_by_check = {}
    has_nonpassed = False
    for index, record in enumerate(verification):
        if not isinstance(record, dict):
            errors.append(f"{label} verification {index} must be an object")
            continue
        for field in ("check", "command", "status", "result"):
            if not isinstance(record.get(field), str) or not record[field].strip():
                errors.append(f"{label} verification {index} needs {field}")
        if record.get("status") not in {"passed", "failed", "blocked"}:
            errors.append(f"{label} verification {index} has invalid status")
        elif record.get("status") != "passed":
            has_nonpassed = True
        check = normalize_check(record.get("check"))
        if not isinstance(check, str) or not check:
            continue
        if check in verification_by_check:
            errors.append(f"{label} verification has duplicate check: {check}")
        else:
            verification_by_check[check] = record
        if accepted and check not in accepted:
            errors.append(f"{label} verification references unknown acceptance check")

    status = change.get("status")
    if status == "proposed" and verification:
        errors.append(f"{label} proposed change must not have verification")
    if status == "implemented":
        missing_checks = accepted - set(verification_by_check)
        if missing_checks:
            errors.append(f"{label} implemented without passed acceptance checks")
        if has_nonpassed:
            errors.append(f"{label} implemented has non-passed verification")
        if not verification:
            errors.append(f"{label} implemented without verification")
    return errors


def decision_review_context(decision_path):
    """返回 decision manifest 所在仓库与 reviews 目录，拒绝非 canonical 路径。"""
    review_root = decision_path.parent
    if review_root.name != "reviews" or review_root.parent.name != "spec":
        raise ValueError("decision manifest must be under spec/reviews")
    if not re.fullmatch(r"history-learn-\d{4}-\d{2}-\d{2}\.json", decision_path.name):
        raise ValueError("decision manifest needs canonical history-learn date name")
    return review_root.parent.parent, review_root


def is_canonical_prior_source(source):
    """旧账本引用必须使用 canonical repository-relative 路径。"""
    if not isinstance(source, str):
        return False
    path = Path(source)
    return (
        not path.is_absolute()
        and len(path.parts) == 3
        and path.parts[:2] == ("spec", "reviews")
        and bool(re.fullmatch(r"history-learn-\d{4}-\d{2}-\d{2}\.json", path.name))
    )


def repository_relative_path_is_safe(repository_root, relative_path):
    if not isinstance(relative_path, str) or not relative_path.strip():
        return False
    path = Path(relative_path)
    if path.is_absolute() or ".." in path.parts:
        return False
    try:
        root = repository_root.resolve()
        resolved = (root / path).resolve()
        resolved.relative_to(root)
    except (OSError, ValueError, RuntimeError, TypeError):
        return False
    return True


def load_prior_decisions(repository_root, review_root, decision_path, project_filter):
    """加载同项目、日期早于当前账本的决策链。"""
    errors = []
    documents = []
    review_fd = None
    try:
        review_fd = open_directory_nofollow(review_root)
        paths = sorted(review_root.glob("history-learn-*.json"))
    except (OSError, ValueError, RuntimeError, TypeError) as error:
        return [], [f"prior decision directory unreadable: {type(error).__name__}"]

    try:
        for path in paths:
            if not re.fullmatch(r"history-learn-\d{4}-\d{2}-\d{2}\.json", path.name):
                continue
            if path.name >= decision_path.name:
                continue
            try:
                entry_stat = os.stat(path.name, dir_fd=review_fd, follow_symlinks=False)
                if stat.S_ISLNK(entry_stat.st_mode):
                    errors.append(f"prior decision must not be a symlink: {path.name}")
                    continue
                payload, _ = read_json_file_at(review_fd, path.name)
            except (
                OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
                json.JSONDecodeError,
            ) as error:
                errors.append(f"prior decision unreadable: {path.name}: {type(error).__name__}")
                continue
            if not isinstance(payload, dict):
                errors.append(f"prior decision must be an object: {path.name}")
                continue
            if payload.get("version") != 1:
                errors.append(f"prior decision has unsupported version: {path.name}")
                continue
            if "project_filter" not in payload:
                errors.append(f"prior decision missing project_filter: {path.name}")
                continue
            if payload.get("project_filter") is not None and not isinstance(payload["project_filter"], str):
                errors.append(f"prior decision has invalid project_filter: {path.name}")
                continue
            if not isinstance(payload.get("run_id"), str) or not payload["run_id"].strip():
                errors.append(f"prior decision missing run_id: {path.name}")
                continue
            if not isinstance(payload.get("source_manifest_sha256"), str) or not re.fullmatch(
                r"[0-9a-f]{64}", payload["source_manifest_sha256"]
            ):
                errors.append(f"prior decision has invalid source_manifest_sha256: {path.name}")
                continue
            if not isinstance(payload.get("changes"), list) or not isinstance(payload.get("prior_attribution"), list):
                errors.append(f"prior decision has invalid change chain: {path.name}")
                continue
            if payload.get("project_filter") != project_filter:
                continue
            source = path.relative_to(repository_root).as_posix()
            documents.append((source, payload))
        return documents, errors
    finally:
        os.close(review_fd)


def validate_unit(run_dir, unit):
    errors = []
    sidecar_sha256 = None
    if not isinstance(unit, dict):
        return ["unit must be an object"], sidecar_sha256

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

    if sidecar_path is None:
        errors.append("sidecar missing")
        return errors, sidecar_sha256
    try:
        run_fd = open_directory_nofollow(run_dir)
        try:
            sidecar_content, sidecar_stat, sidecar_sha256 = read_file_at(
                run_fd,
                unit.get("sidecar_path"),
            )
        finally:
            os.close(run_fd)
        sidecar_text = sidecar_content.decode("utf-8")
        sidecar = json.loads(sidecar_text)
    except (
        OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
        json.JSONDecodeError,
    ) as error:
        errors.append(f"sidecar unreadable: {type(error).__name__}")
        return errors, None
    if sidecar_stat.st_mode & 0o077:
        errors.append("sidecar permissions are not private")
    if not isinstance(sidecar, dict):
        errors.append("sidecar must be an object")
        return errors, sidecar_sha256
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
        return errors, sidecar_sha256
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
        required = {
            "id", "classification", "failure_pattern", "root_cause", "evidence",
            "counterevidence", "frequency", "impact", "confidence", "fact_source",
            "target_surface", "why_this_surface", "predicted_fixes", "risk_regressions",
            "acceptance",
        }
        allowed_classifications = ALLOWED_CLASSIFICATIONS
        allowed_surfaces = ALLOWED_SURFACES
        seen_finding_ids = set()
        for index, finding in enumerate(findings):
            if not isinstance(finding, dict):
                errors.append(f"finding {index} must be an object")
                continue
            missing_fields = sorted(required - set(finding))
            if missing_fields:
                errors.append(f"finding {index} missing: {', '.join(missing_fields)}")
            finding_id = finding.get("id")
            if not has_stable_id(finding_id, "F"):
                errors.append(f"finding {index} needs stable F-* id")
            elif finding_id in seen_finding_ids:
                errors.append(f"duplicate finding id: {finding_id}")
            else:
                seen_finding_ids.add(finding_id)
            if finding.get("classification") not in allowed_classifications:
                errors.append(f"finding {index} has invalid classification")
            for field in ("failure_pattern", "root_cause", "why_this_surface"):
                if not isinstance(finding.get(field), str) or not finding[field].strip():
                    errors.append(f"finding {index} needs {field}")
            evidence = finding.get("evidence")
            if not isinstance(evidence, list) or not evidence:
                errors.append(f"finding {index} needs evidence")
            elif not all(has_evidence_locator(reference, expected) for reference in evidence):
                errors.append(f"finding {index} evidence needs an input path and locator")
            counterevidence = finding.get("counterevidence")
            if not isinstance(counterevidence, list):
                errors.append(f"finding {index} counterevidence must be a list")
            elif not all(has_evidence_locator(reference, expected) for reference in counterevidence):
                errors.append(f"finding {index} counterevidence needs an input path and locator")
            if not re.search(r"\d+\s*/\s*\d+", str(finding.get("frequency", ""))):
                errors.append(f"finding {index} frequency needs a denominator")
            if finding.get("impact") not in {"high", "medium", "low"}:
                errors.append(f"finding {index} has invalid impact")
            if finding.get("confidence") not in {"high", "medium", "low"}:
                errors.append(f"finding {index} has invalid confidence")
            if not str(finding.get("fact_source", "")).strip():
                errors.append(f"finding {index} needs fact_source")
            if finding.get("target_surface") not in allowed_surfaces:
                errors.append(f"finding {index} has invalid target_surface")
            for field in ("predicted_fixes", "risk_regressions"):
                value = finding.get(field)
                if not isinstance(value, list) or not value or not all(
                    isinstance(item, str) and item.strip() for item in value
                ):
                    errors.append(f"finding {index} needs non-empty {field}")
            acceptance_errors, _ = acceptance_contract(finding, f"finding {index}")
            errors.extend(acceptance_errors)
    return errors, sidecar_sha256


def validate_run(run_dir):
    try:
        run_dir = Path(run_dir).expanduser().resolve()
        run_fd = open_directory_nofollow(run_dir)
        try:
            manifest_content, manifest_stat, manifest_sha256 = read_file_at(run_fd, "manifest.json")
        finally:
            os.close(run_fd)
        manifest_text = manifest_content.decode("utf-8")
        manifest = json.loads(manifest_text)
    except (
        OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
        json.JSONDecodeError,
    ) as error:
        return {
            "status": "failed",
            "errors": [f"manifest unreadable: {type(error).__name__}"],
            "units": [],
            "attestation": None,
        }
    manifest_path = run_dir / "manifest.json"
    if not isinstance(manifest, dict):
        return {
            "status": "failed",
            "errors": ["manifest must be an object"],
            "units": [],
            "attestation": None,
        }

    errors = []
    if has_group_or_world_permissions(run_dir):
        errors.append("run directory permissions are not private")
    if manifest_stat.st_mode & 0o077:
        errors.append("manifest permissions are not private")
    if contains_sensitive_credential(manifest_text):
        errors.append("manifest contains sensitive credential pattern")
    if manifest.get("version") != MANIFEST_VERSION:
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
    sidecar_attestation = []
    seen_unit_ids = set()
    unit_paths = []
    for unit in units:
        unit_id = unit.get("id") if isinstance(unit, dict) else None
        if isinstance(unit, dict) and isinstance(unit.get("inputs"), list):
            unit_paths.extend(item.get("path") for item in unit["inputs"] if isinstance(item, dict))
        sidecar_sha256 = None
        if not isinstance(unit_id, str) or not unit_id:
            unit_errors = ["unit id missing"]
        elif unit_id in seen_unit_ids:
            unit_errors = [f"duplicate unit id: {unit_id}"]
        else:
            seen_unit_ids.add(unit_id)
            unit_errors, sidecar_sha256 = validate_unit(run_dir, unit)
        unit_reports.append({"unit_id": unit_id, "status": "passed" if not unit_errors else "failed", "errors": unit_errors})
        if sidecar_sha256 is not None:
            sidecar_attestation.append({
                "unit_id": unit_id,
                "path": unit.get("sidecar_path"),
                "sha256": sidecar_sha256,
            })
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
    status = "passed" if not errors else "failed"
    attestation = None
    if status == "passed":
        attestation = {
            "manifest_sha256": manifest_sha256,
            "sidecars": sidecar_attestation,
        }
    return {
        "status": status,
        "errors": errors,
        "units": unit_reports,
        "attestation": attestation,
    }


def validate_decision_manifest(run_dir, decision_manifest_path, repository_root_override=None):
    """校验跨轮决策账本可追溯到当前 snapshot run 的 finding。"""
    try:
        run_dir = Path(run_dir).expanduser().resolve()
        decision_path = Path(decision_manifest_path).expanduser().resolve()
    except (OSError, ValueError, RuntimeError, TypeError) as error:
        return {
            "status": "failed",
            "path": str(decision_manifest_path),
            "errors": [f"decision path unreadable: {type(error).__name__}"],
        }
    errors = []

    try:
        run_fd = open_directory_nofollow(run_dir)
        try:
            manifest_content, _, manifest_sha256 = read_file_at(run_fd, "manifest.json")
        finally:
            os.close(run_fd)
        review_fd = open_directory_nofollow(decision_path.parent)
        try:
            decision_content, _, decision_sha256 = read_file_at(review_fd, decision_path.name)
        finally:
            os.close(review_fd)
        manifest = json.loads(manifest_content.decode("utf-8"))
        decision_text = decision_content.decode("utf-8")
        decision = json.loads(decision_text)
    except (
        OSError, ValueError, RuntimeError, TypeError, UnicodeDecodeError,
        json.JSONDecodeError,
    ) as error:
        return {
            "status": "failed",
            "path": str(decision_path),
            "errors": [f"decision manifest unreadable: {type(error).__name__}"],
        }

    if not isinstance(manifest, dict):
        return {"status": "failed", "path": str(decision_path), "errors": ["run manifest must be an object"]}
    if not isinstance(decision, dict):
        return {"status": "failed", "path": str(decision_path), "errors": ["decision manifest must be an object"]}
    try:
        repository_root, review_root = decision_review_context(decision_path)
    except ValueError as error:
        errors.append(str(error))
        repository_root = None
        review_root = None
    try:
        manifest_repository_root = (
            str(Path(repository_root_override).expanduser().resolve())
            if repository_root_override is not None
            else manifest.get("repository_root") or manifest.get("project_filter")
        )
        resolved_manifest_repository_root = (
            Path(manifest_repository_root).expanduser().resolve()
            if isinstance(manifest_repository_root, str) and manifest_repository_root
            else None
        )
    except (OSError, ValueError, RuntimeError, TypeError):
        manifest_repository_root = None
        resolved_manifest_repository_root = None
    if resolved_manifest_repository_root is None:
        errors.append("run manifest repository root is unavailable")
    elif repository_root is not None and repository_root != resolved_manifest_repository_root:
        errors.append("decision manifest is outside the run repository")
    if contains_sensitive_credential(decision_text):
        errors.append("decision manifest contains sensitive credential pattern")
    if decision.get("version") != 1:
        errors.append("unsupported decision manifest version")
    if decision.get("run_id") != manifest.get("run_id"):
        errors.append("decision run_id mismatch")
    if decision.get("source_run_dir") != str(run_dir):
        errors.append("decision source_run_dir mismatch")
    if decision.get("source_manifest_sha256") != manifest_sha256:
        errors.append("decision source manifest digest mismatch")
    if decision.get("project_filter") != manifest.get("project_filter"):
        errors.append("decision project_filter mismatch")

    run_report = validate_run(run_dir)
    if run_report.get("status") != "passed":
        return {
            "status": "failed",
            "path": str(decision_path),
            "sha256": decision_sha256,
            "errors": ["run must pass validation before decision validation"],
        }
    run_attestation = run_report.get("attestation")
    if not isinstance(run_attestation, dict):
        errors.append("run attestation missing")
    else:
        if decision.get("source_manifest_sha256") != run_attestation.get("manifest_sha256"):
            errors.append("decision source manifest digest mismatch")
        if decision.get("source_sidecars") != run_attestation.get("sidecars"):
            errors.append("decision source sidecar attestation mismatch")

    finding_contracts = load_finding_contracts(
        run_dir,
        manifest,
        expected_sidecars=run_attestation.get("sidecars") if isinstance(run_attestation, dict) else [],
    )
    if finding_contracts is None:
        errors.append("run sidecars changed after validation")
        finding_contracts = {}
    finding_refs = set(finding_contracts)

    prior_documents = []
    if repository_root is not None and review_root is not None:
        prior_documents, prior_errors = load_prior_decisions(
            repository_root,
            review_root,
            decision_path,
            decision.get("project_filter"),
        )
        errors.extend(prior_errors)

    prior_changes = {}
    terminal_attributions = set()
    for source, payload in prior_documents:
        historical_changes = payload.get("changes", [])
        if not isinstance(historical_changes, list):
            errors.append(f"prior decision changes must be a list: {source}")
            continue
        for historical_change in historical_changes:
            if not isinstance(historical_change, dict):
                errors.append(f"prior decision has invalid change: {source}")
                continue
            change_id = historical_change.get("id")
            if not has_stable_id(change_id, "CHG"):
                errors.append(f"prior decision change needs stable CHG-* id: {source}")
                continue
            key = (source, change_id)
            if key in prior_changes:
                errors.append(f"duplicate prior change identity: {source}#{change_id}")
            prior_changes[key] = historical_change
        historical_attributions = payload.get("prior_attribution", [])
        if not isinstance(historical_attributions, list):
            errors.append(f"prior decision attribution must be a list: {source}")
            continue
        attested_findings = load_attested_historical_findings(repository_root, source, payload)
        for historical_attribution in historical_attributions:
            if historical_terminal_verdict_is_valid(
                historical_attribution,
                source,
                payload,
                prior_changes,
                attested_findings,
            ):
                key = (historical_attribution["source"], historical_attribution["change_id"])
                terminal_attributions.add(key)

    eligible_prior = set()
    for key, historical_change in prior_changes.items():
        if historical_change.get("status") != "implemented" or key in terminal_attributions:
            continue
        verification_errors = verification_contract_errors(
            historical_change,
            f"prior change {key[0]}#{key[1]}",
        )
        if verification_errors:
            errors.extend(verification_errors)
            continue
        eligible_prior.add(key)

    prior = decision.get("prior_attribution")
    attributed_prior = set()
    seen_attribution_keys = set()
    if not isinstance(prior, list):
        errors.append("prior_attribution must be a list")
    else:
        allowed_verdicts = {"keep", "improve", "revert", "inconclusive", "not_implemented"}
        for index, attribution in enumerate(prior):
            if not isinstance(attribution, dict):
                errors.append(f"prior attribution {index} must be an object")
                continue
            source = attribution.get("source")
            for field in ("source", "change_id", "rationale"):
                if not isinstance(attribution.get(field), str) or not attribution[field].strip():
                    errors.append(f"prior attribution {index} needs {field}")
            if source is not None and not is_canonical_prior_source(source):
                errors.append(f"prior attribution {index} has noncanonical source")
            verdict = attribution.get("verdict")
            if verdict not in allowed_verdicts:
                errors.append(f"prior attribution {index} has invalid verdict")
            key = (attribution.get("source"), attribution.get("change_id"))
            if key in seen_attribution_keys:
                errors.append(f"duplicate prior attribution: {key[0]}#{key[1]}")
            else:
                seen_attribution_keys.add(key)
            historical_change = prior_changes.get(key)
            if key in terminal_attributions:
                errors.append(f"prior attribution {index} repeats terminal attribution")
            if historical_change is None:
                errors.append(f"prior attribution {index} references unknown change")
            elif verdict == "not_implemented":
                if historical_change.get("status") == "implemented":
                    errors.append(f"prior attribution {index} marks an implemented change as not_implemented")
            else:
                if historical_change.get("status") != "implemented":
                    errors.append(f"prior attribution {index} requires an implemented change")
                attributed_prior.add(key)
            errors.extend(attribution_verdict_errors(
                attribution,
                historical_change,
                finding_contracts,
                decision.get("run_id"),
                decision.get("source_manifest_sha256"),
                f"prior attribution {index}",
            ))

    missing_attributions = sorted(eligible_prior - attributed_prior)
    for source, change_id in missing_attributions:
        errors.append(f"eligible prior change not attributed: {source}#{change_id}")

    changes = decision.get("changes")
    if not isinstance(changes, list):
        errors.append("changes must be a list")
        changes = []
    seen_change_ids = set()
    allowed_statuses = {"proposed", "implemented", "blocked"}
    allowed_classifications = ALLOWED_CLASSIFICATIONS
    allowed_surfaces = ALLOWED_SURFACES
    required = {
        "id", "status", "source_findings", "classification", "failure_pattern", "root_cause",
        "baseline", "target_surface", "files", "why_this_surface", "predicted_fixes", "risk_regressions",
        "acceptance", "verification",
    }
    for index, change in enumerate(changes):
        if not isinstance(change, dict):
            errors.append(f"change {index} must be an object")
            continue
        missing_fields = sorted(required - set(change))
        if missing_fields:
            errors.append(f"change {index} missing: {', '.join(missing_fields)}")
        change_id = change.get("id")
        if not has_stable_id(change_id, "CHG"):
            errors.append(f"change {index} needs stable CHG-* id")
        elif change_id in seen_change_ids:
            errors.append(f"duplicate change id: {change_id}")
        else:
            seen_change_ids.add(change_id)
        if change.get("status") not in allowed_statuses:
            errors.append(f"change {index} has invalid status")
        if change.get("classification") not in allowed_classifications:
            errors.append(f"change {index} has invalid classification")
        if change.get("target_surface") not in allowed_surfaces:
            errors.append(f"change {index} has invalid target_surface")
        for field in ("failure_pattern", "root_cause", "baseline", "why_this_surface"):
            if not isinstance(change.get(field), str) or not change[field].strip():
                errors.append(f"change {index} needs {field}")
        source_findings = change.get("source_findings")
        source_contracts = []
        if not isinstance(source_findings, list) or not source_findings:
            errors.append(f"change {index} needs source_findings")
        elif not all(isinstance(reference, str) and reference in finding_refs for reference in source_findings):
            errors.append(f"change {index} references unknown finding")
        else:
            source_contracts = [finding_contracts[reference] for reference in source_findings]
            if any(source.get("classification") != change.get("classification") for source in source_contracts):
                errors.append(f"change {index} classification differs from source finding")
            if any(source.get("target_surface") != change.get("target_surface") for source in source_contracts):
                errors.append(f"change {index} target_surface differs from source finding")
        files = change.get("files")
        if not isinstance(files, list) or not all(
            isinstance(item, str) and item.strip() for item in files
        ):
            errors.append(f"change {index} files must be a string list")
        elif not files and change.get("target_surface") not in {"external", "none"}:
            errors.append(f"change {index} needs non-empty files")
        for field in ("predicted_fixes", "risk_regressions"):
            value = change.get(field)
            if not isinstance(value, list) or not value or not all(
                isinstance(item, str) and item.strip() for item in value
            ):
                errors.append(f"change {index} needs non-empty {field}")
        if isinstance(files, list):
            for file_path in files:
                if not isinstance(file_path, str):
                    continue
                if repository_root is None or not repository_relative_path_is_safe(repository_root, file_path):
                    errors.append(f"change {index} file is not repository-relative: {file_path}")
        acceptance_errors, _ = acceptance_contract(change, f"change {index}")
        errors.extend(acceptance_errors)
        if source_contracts and isinstance(change.get("acceptance"), dict):
            for kind in ("target", "preserved_success"):
                source_checks = {
                    check
                    for source in source_contracts
                    for check in source.get("acceptance", {}).get(kind, [])
                    if isinstance(check, str)
                }
                decision_checks = change["acceptance"].get(kind, [])
                if isinstance(decision_checks, list) and not source_checks.issubset(decision_checks):
                    errors.append(f"change {index} acceptance omits source finding {kind} checks")
        errors.extend(verification_contract_errors(change, f"change {index}"))

    return {
        "status": "passed" if not errors else "failed",
        "path": str(decision_path),
        "sha256": decision_sha256,
        "errors": errors,
    }


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
    parser.add_argument("--decision-manifest", help="可选的跨轮决策账本 JSON 路径")
    parser.add_argument(
        "--repository-root",
        help="旧 v1 --all run 缺 repository_root 时显式提供本次账本所属仓库根",
    )
    parser.add_argument("--cleanup-inputs", action="store_true", help="验证通过后删除 snapshot、提取物和 prompts")
    return parser.parse_args()


def main():
    args = parse_args()
    run_dir = Path(args.run_dir).expanduser().resolve()
    report = validate_run(run_dir)
    if args.decision_manifest:
        decision_report = validate_decision_manifest(
            run_dir,
            args.decision_manifest,
            repository_root_override=args.repository_root,
        )
        report["decision_manifest"] = decision_report
        if decision_report["status"] != "passed":
            report["status"] = "failed"
            report["errors"].append("decision manifest failed validation")
    write_private_json(run_dir / "validation.json", report)
    for unit in report["units"]:
        print(f"{unit['unit_id']}: {unit['status']}")
        for error in unit["errors"]:
            print(f"  - {error}")
    decision_report = report.get("decision_manifest")
    if decision_report:
        print(f"Decision manifest: {decision_report['status']}")
        for error in decision_report["errors"]:
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
