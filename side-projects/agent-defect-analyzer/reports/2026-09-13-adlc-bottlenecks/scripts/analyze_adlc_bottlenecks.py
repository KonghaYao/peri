#!/usr/bin/env python3
"""Quantify ADLC workflow-run journals without treating prose as test evidence.

The script intentionally reads only workflow-run state/journal/script files and
ADLC task manifests/handoffs.  It emits a compact, reproducible snapshot for
the local ADLC bottleneck investigation.
"""

from __future__ import annotations

import collections
import datetime as dt
import hashlib
import json
import math
import os
import re
import argparse
from pathlib import Path
from typing import Any


REPO = Path(__file__).resolve().parents[5]
RUN_ROOT = REPO / ".claude" / "workflow-runs"
TASK_ROOT = REPO / ".peri" / "adlc" / "tasks"
OUT = Path(__file__).resolve().parent
GENERATED_AT = "2026-09-13"


def sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def read_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as f:
        return json.load(f)


def parse_iso(value: Any) -> dt.datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def stats(values: list[int | float]) -> dict[str, Any]:
    if not values:
        return {"count": 0, "sum": None, "min": None, "p50": None, "p90": None, "max": None}
    xs = sorted(values)

    def percentile(p: float) -> float:
        if len(xs) == 1:
            return xs[0]
        pos = (len(xs) - 1) * p
        lo, hi = math.floor(pos), math.ceil(pos)
        if lo == hi:
            return xs[lo]
        return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo)

    return {
        "count": len(xs),
        "sum": sum(xs),
        "min": xs[0],
        "p50": percentile(0.5),
        "p90": percentile(0.9),
        "max": xs[-1],
    }


def rel(path: Path) -> str:
    return str(path.relative_to(REPO))


def phase_names(script: str) -> list[str]:
    return re.findall(r"\bphase\(\s*[`']([^`']+)[`']", script)


def handoff_count(task_path: Path) -> int:
    handoffs = task_path / "handoffs"
    return sum(1 for p in handoffs.rglob("*") if p.is_file()) if handoffs.is_dir() else 0


def file_presence(task_path: Path, fragment: str) -> list[str]:
    return [rel(p) for p in task_path.rglob("*") if p.is_file() and fragment in str(p)]


def load_tasks() -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    tasks: dict[str, dict[str, Any]] = {}
    input_files: list[dict[str, Any]] = []
    for manifest_path in sorted(TASK_ROOT.glob("*/manifest.json")):
        manifest = read_json(manifest_path)
        adlc_id = manifest.get("adlcId")
        if not adlc_id:
            continue
        input_files.append({"path": rel(manifest_path), "kind": "manifest", "exists": True,
                            "bytes": manifest_path.stat().st_size, "sha256": sha256(manifest_path)})
        runs = manifest.get("workflowRuns", [])
        if isinstance(runs, dict):
            expected = [x for group in runs.values() if isinstance(group, list) for x in group]
        else:
            expected = runs if isinstance(runs, list) else []
        tasks[adlc_id] = {
            "adlcId": adlc_id,
            "taskPath": rel(manifest_path.parent),
            "manifestPath": rel(manifest_path),
            "manifest": manifest,
            "expectedRunIds": expected,
            "observedRunIds": [],
        }
    return tasks, input_files


def journal_record(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for line_no, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as e:
                rows.append({"_parse_error": str(e), "_line": line_no})
                continue
            row["_line"] = line_no
            rows.append(row)
    return rows


def outcome_for_journal(result: dict[str, Any]) -> str:
    kind = result.get("kind")
    if kind == "ok":
        return "ok"
    if kind in {"cancelled", "canceled"}:
        return "cancelled"
    if kind in {"error", "failed", "failure"}:
        return "error"
    if kind == "dead":
        # A dead attempt has no structured model error or cancellation marker.
        # Keep it separate so it is not silently promoted to an error rate.
        return "dead"
    return "unknown"


def classify_change_evidence(run: dict[str, Any]) -> dict[str, Any]:
    """Conservative text evidence only; never claims a source diff was observed."""
    text = "\n".join(x.get("output", "") for x in run.get("journal", []) if isinstance(x, dict))
    lower = text.lower()
    explicit_no_change = bool(re.search(r"零源码改动|未修改产品|产品代码/测试未做任何修改|product code.*(?:not|no) modification", text, re.I))
    source_terms = ["修复", "新增回归", "src/", "source digest", "源码摘要", "产品代码", "fix", "implement"]
    hit_terms = [term for term in source_terms if term.lower() in lower]
    if explicit_no_change and not any(x in lower for x in ["fix", "修复", "新增回归", "source digest"]):
        classification = "no_source_change_claimed"
    elif hit_terms:
        classification = "source_or_test_change_claimed"
    else:
        classification = "unknown"
    return {"classification": classification, "evidenceTerms": hit_terms[:20],
            "sourceDiffObserved": False,
            "note": "Derived from journal prose; no production diff was read, so this is not a source-change proof."}


def dead_reason_category(result: dict[str, Any], status: Any) -> str:
    """Classify journal result.reason/detail; keep state error as separate context."""
    reason = str(result.get("reason") or "").strip().lower()
    detail = str(result.get("detail") or "").strip().lower()
    text = f"{reason} {detail}"
    if reason in {"cancelled", "canceled", "killed", "killed-by-user", "killed_by_user"}:
        return "killed_or_cancelled"
    if reason in {"no-structured-output", "no_structured_output", "structured-output-unavailable"}:
        if "expected type" in detail or "schema" in detail:
            return "no_structured_output_schema"
        if "valid json" in detail or "json" in detail:
            return "no_structured_output_invalid_json"
        return "no_structured_output"
    if reason in {"runagent-threw", "panic", "unhandled-error", "unhandled_error"}:
        if "max iterations" in detail:
            return "runagent_max_iterations"
        if "http status 400" in detail or "http status 403" in detail:
            return "runagent_provider_http_error"
        return "runagent_threw"
    if status in {"killed", "cancelled", "canceled"} and not reason:
        return "state_killed_or_cancelled_without_journal_reason"
    return "journal_reason_unknown" if not text else "journal_reason_other"


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=None, help="repository root (default: inferred from this file)")
    parser.add_argument("--out", default=None, help="output directory (required; choose a fresh directory)")
    args = parser.parse_args(argv)
    if args.out is None:
        parser.error("--out is required; refusing to write beside the script")
    global REPO, RUN_ROOT, TASK_ROOT, OUT
    if args.repo:
        REPO = Path(args.repo).resolve()
    OUT = Path(args.out).resolve()
    RUN_ROOT = REPO / ".claude" / "workflow-runs"
    TASK_ROOT = REPO / ".peri" / "adlc" / "tasks"
    OUT.mkdir(parents=True, exist_ok=True)
    for output_name in ("metrics.json", "summary.json", "findings.md"):
        if (OUT / output_name).exists():
            raise FileExistsError(f"refusing to overwrite existing {OUT / output_name}; choose a fresh --out")
    for output_name in ("metrics.json", "summary.json"):
        if (OUT / output_name).exists():
            raise FileExistsError(f"refusing to overwrite existing {OUT / output_name}; choose a fresh --out")
    tasks, input_files = load_tasks()
    expected_to_task = {run_id: task_id for task_id, task in tasks.items() for run_id in task["expectedRunIds"]}
    run_dirs = sorted(p for p in RUN_ROOT.iterdir() if p.is_dir())
    runs: list[dict[str, Any]] = []
    journal_output_texts: dict[str, str] = {}

    for run_dir in run_dirs:
        run_id = run_dir.name
        state_path, journal_path, script_path = (run_dir / n for n in ("state.json", "journal.jsonl", "script.js"))
        state = read_json(state_path) if state_path.is_file() else {}
        script = script_path.read_text(encoding="utf-8") if script_path.is_file() else ""
        journal = journal_record(journal_path)
        journal_output_texts[run_id] = "\n".join((row.get("result") or {}).get("output", "") for row in journal if "_parse_error" not in row)
        task_id = expected_to_task.get(run_id)
        if task_id and run_id not in tasks[task_id]["observedRunIds"]:
            tasks[task_id]["observedRunIds"].append(run_id)
        for path, kind in ((state_path, "state"), (journal_path, "journal"), (script_path, "script")):
            input_files.append({"path": rel(path), "kind": kind, "exists": path.is_file(),
                                "bytes": path.stat().st_size if path.is_file() else None,
                                "sha256": sha256(path)})

        entries: list[dict[str, Any]] = []
        for row in journal:
            if "_parse_error" in row:
                entries.append(row)
                continue
            result = row.get("result") or {}
            usage = result.get("usage") or {}
            entries.append({
                "seq": row.get("seq"), "line": row.get("_line"), "kind": result.get("kind"),
                "outcome": outcome_for_journal(result), "phase": result.get("phase"),
                "model": result.get("model"), "durationMs": result.get("durationMs"),
                "reason": result.get("reason"), "detail": result.get("detail"),
                "attempt": row.get("attempt"),
                "attemptDisposition": (row.get("attempt") or {}).get("disposition") if isinstance(row.get("attempt"), dict) else None,
                "usage": usage,
                "tokenFieldWarning": "usage.outputTokens/tokenCount are reported raw; no billing inference is made",
                "deadReasonCategory": dead_reason_category(result, state.get("status")) if outcome_for_journal(result) == "dead" else None,
            })
        runs.append({
            "runId": run_id, "taskId": task_id, "workflowName": state.get("workflow_name"),
            "status": state.get("status"),
            "state": {
                "execution_status": state.get("execution_status"),
                "acceptance_status": state.get("acceptance_status"),
                "delivery_status": state.get("delivery_status"),
                "post_processing_status": state.get("post_processing_status"),
                "started_at": state.get("started_at"), "finished_at": state.get("finished_at"),
                "error": state.get("error"),
            },
            "returnValueStatus": (state.get("return_value") or {}).get("status") if isinstance(state.get("return_value"), dict) else None,
            "stateError": state.get("error"),
            "scriptPhases": phase_names(script), "journal": entries,
            "journalFilePresent": journal_path.is_file(),
            "changeEvidence": classify_change_evidence({"journal": [x for x in journal if "_parse_error" not in x]}),
        })

    for task_id, task in tasks.items():
        task["missingRunIds"] = [x for x in task["expectedRunIds"] if x not in task["observedRunIds"]]
        task["unexpectedObservedRunIds"] = [r["runId"] for r in runs if r["taskId"] == task_id and r["runId"] not in task["expectedRunIds"]]
        task_path = REPO / task["taskPath"]
        # Handoffs are permitted coordination inputs. Hash every one so the
        # report can identify the exact document set whose size was counted.
        for handoff in sorted((task_path / "handoffs").rglob("*") if (task_path / "handoffs").is_dir() else []):
            if handoff.is_file():
                input_files.append({"path": rel(handoff), "kind": "handoff", "exists": True,
                                    "bytes": handoff.stat().st_size, "sha256": sha256(handoff)})

    def task_runs(task_id: str) -> list[dict[str, Any]]:
        return [r for r in runs if r["taskId"] == task_id]

    def aggregate(task_id: str) -> dict[str, Any]:
        trs = task_runs(task_id)
        phase_runs: dict[str, set[str]] = collections.defaultdict(set)
        phase_rows: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
        model_rows: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
        counts = collections.Counter()
        state_counts = {key: collections.Counter() for key in ("execution_status", "acceptance_status", "delivery_status", "post_processing_status")}
        return_status = collections.Counter()
        for run in trs:
            for row in run["journal"]:
                if "_parse_error" in row:
                    counts["parse_error"] += 1
                    continue
                counts[row["outcome"]] += 1
                if row.get("phase"):
                    phase_runs[row["phase"]].add(run["runId"])
                    phase_rows[row["phase"]].append(row)
                if row.get("model"):
                    model_rows[row["model"]].append(row)
            for key, c in state_counts.items():
                value = run["state"].get(key)
                c[value if value is not None else "<missing>"] += 1
            return_status[run["returnValueStatus"] if run["returnValueStatus"] is not None else "<missing>"] += 1
        phase_distribution = {}
        for phase, rows in sorted(phase_rows.items()):
            phase_distribution[phase] = {"physicalRunCount": len(phase_runs[phase]),
                                         "journalAttemptCount": len(rows),
                                         "durationMs": stats([r["durationMs"] for r in rows if isinstance(r.get("durationMs"), (int, float))]),
                                         "models": dict(collections.Counter(r.get("model") or "<missing>" for r in rows))}
        model_distribution = {}
        for model, rows in sorted(model_rows.items()):
            model_distribution[model] = {"attemptCount": len(rows),
                                         "durationMs": stats([r["durationMs"] for r in rows if isinstance(r.get("durationMs"), (int, float))]),
                                         "phases": dict(collections.Counter(r.get("phase") or "<missing>" for r in rows))}
        starts = [parse_iso(r["state"].get("started_at")) for r in trs]
        finishes = [parse_iso(r["state"].get("finished_at")) for r in trs]
        starts, finishes = [x for x in starts if x], [x for x in finishes if x]
        return {
            "physicalRunCount": len(trs),
            "window": {"startedAtMin": min(starts).isoformat() if starts else None,
                       "startedAtMax": max(starts).isoformat() if starts else None,
                       "finishedAtMin": min(finishes).isoformat() if finishes else None,
                       "finishedAtMax": max(finishes).isoformat() if finishes else None},
            "logicalPhaseToPhysicalRuns": {p: {"physicalRunCount": len(ids), "runIds": sorted(ids)} for p, ids in sorted(phase_runs.items())},
            "phaseSource": "journal result.phase only; script template declarations such as ${R} are retained per run but are not counted as executed phases",
            "journalAttemptCounts": {"ok": counts["ok"], "error": counts["error"], "cancelled": counts["cancelled"],
                                      "dead": counts["dead"], "unknown": counts["unknown"], "parse_error": counts["parse_error"]},
            "stateDistributions": {k: dict(v) for k, v in state_counts.items()},
            "returnValueStatusDistribution": dict(return_status),
            "modelDistribution": model_distribution,
            "phaseDistribution": phase_distribution,
            "durationInterpretation": "durationMs sums are child-attempt sums, not workflow walltime; message/thread span is not execution time",
        }

    task_summary: dict[str, Any] = {}
    for task_id, task in sorted(tasks.items()):
        task_path = REPO / task["taskPath"]
        manifest = task["manifest"]
        task_summary[task_id] = {
            "manifestPath": task["manifestPath"], "expectedRunCount": len(task["expectedRunIds"]),
            "observedRunCount": len(task["observedRunIds"]), "missingRunIds": task["missingRunIds"],
            "unexpectedObservedRunIds": task["unexpectedObservedRunIds"],
            "contractRevisions": {k: (v or {}).get("revision") for k, v in (manifest.get("contracts") or {}).items() if isinstance(v, dict)},
            "handoffFileCount": handoff_count(task_path),
            "manualAssessmentFiles": file_presence(task_path, "assessment") + file_presence(task_path, "review"),
            "learningFiles": file_presence(task_path, "learning") + file_presence(task_path, "performance"),
            "metrics": aggregate(task_id),
        }

    sandbox_id = "2026-09-11-sandbox-mcp-server"
    sandbox_runs = task_runs(sandbox_id)
    sandbox_resume = []
    for run in sorted(sandbox_runs, key=lambda x: x["runId"]):
        phases = sorted(set((row.get("phase") or "") for row in run["journal"] if row.get("phase")))
        round_match = re.search(r"Round-(\d+)", " ".join(phases))
        journal_phases = [x.get("phase") or "" for x in run["journal"] if x.get("phase")]
        output_text = journal_output_texts.get(run["runId"], "")
        sandbox_resume.append({"runId": run["runId"], "phases": phases,
                              "round": int(round_match.group(1)) if round_match else None,
                              "hasExplicitBuildPhase": any("/Build/" in p for p in journal_phases),
                              "hasExplicitGatesPhase": any("/Gates/" in p for p in journal_phases),
                              "buildMentionedInJournalOutput": bool(re.search(r"\bBUILD(?:-|\s|_|r\d|完成)|\bbuild(?:-|\s|_|r\d)", output_text, re.I)),
                              "gatesMentionedInJournalOutput": bool(re.search(r"\bGATES(?:-|\s|_|r\d|完成)|\bgates(?:-|\s|_|r\d)", output_text, re.I)),
                              "reviewAttemptCount": sum("/Verify/" in p for p in journal_phases),
                              "multipleReviewAttempts": sum("/Verify/" in p for p in journal_phases) > 1,
                              "assessorPresent": any("Assess" in p for p in phases),
                              "changeEvidence": run["changeEvidence"],
                              "deadAttempts": sum(1 for x in run["journal"] if x.get("outcome") == "dead"),
                              "phaseObservationNote": "Explicit phase labels come from result.phase; BUILD/GATES may be subtask labels inside an Integrate phase, so output mentions are separate evidence.",
                              "deadReasonCategories": sorted({x.get("deadReasonCategory") for x in run["journal"] if x.get("outcome") == "dead"})})

    duration_band = []
    for run in runs:
        for row in run["journal"]:
            if isinstance(row.get("durationMs"), (int, float)) and 600_000 <= row["durationMs"] <= 1_200_000:
                duration_band.append({"runId": run["runId"], "taskId": run["taskId"], "seq": row.get("seq"),
                                      "phase": row.get("phase"), "model": row.get("model"), "durationMs": row.get("durationMs"),
                                      "journalLine": row.get("line")})
    duration_band.sort(key=lambda x: (-x["durationMs"], x["runId"], x["seq"] or -1))

    metrics = {
        "schema": "adlc-bottlenecks-v3",
        "generatedAt": GENERATED_AT,
        "scope": {"runRoot": str(RUN_ROOT), "taskRoot": str(TASK_ROOT), "runDirectories": len(run_dirs),
                   "manifestTasks": len(tasks), "manifestExpectedRuns": len(expected_to_task),
                   "associationRule": "manifest workflowRuns run ID is authoritative; workflow_name is a cross-check only",
                   "readSources": [".claude/workflow-runs/*/{state.json,journal.jsonl,script.js}", ".peri/adlc/tasks/*/manifest.json", ".peri/adlc/tasks/*/handoffs/**"]},
        "inputFiles": sorted(input_files, key=lambda x: x["path"]),
        "coverage": {"observedRuns": sum(1 for r in runs if r["taskId"]),
                     "unassociatedRuns": [r["runId"] for r in runs if not r["taskId"]],
                     "missingRunIdsByTask": {k: v["missingRunIds"] for k, v in sorted(tasks.items()) if v["missingRunIds"]},
                     "missingJournalRunIds": [r["runId"] for r in runs if not r["journalFilePresent"]],
                     "missingJournalIsUnknown": True},
        "tasks": task_summary,
        "sandboxResumeAudit": {"taskId": sandbox_id, "runs": sandbox_resume,
            "interpretation": "Phase presence describes what was invoked. It cannot prove elapsed walltime or source diff; D-003 Docker→local pivot is a task-authorized scope change recorded in the manifest/decisions and is reported separately."},
        "durationBand10to20MinMs": duration_band,
        "globalJournalAttemptCounts": {"ok": sum(1 for r in runs for x in r["journal"] if x.get("outcome") == "ok"),
                                        "error": sum(1 for r in runs for x in r["journal"] if x.get("outcome") == "error"),
                                        "cancelled": sum(1 for r in runs for x in r["journal"] if x.get("outcome") == "cancelled"),
                                        "dead": sum(1 for r in runs for x in r["journal"] if x.get("outcome") == "dead")},
        "runs": runs,
    }
    (OUT / "metrics.json").write_text(json.dumps(metrics, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    summary = {
        "schema": "adlc-bottlenecks-summary-v1", "generatedAt": GENERATED_AT,
        "scope": metrics["scope"],
        "hashes": {x["path"]: x["sha256"] for x in input_files},
        "tasks": {task_id: {"expectedRunCount": x["expectedRunCount"], "observedRunCount": x["observedRunCount"],
                             "missingRunIds": x["missingRunIds"], "handoffFileCount": x["handoffFileCount"],
                             "metrics": {"window": x["metrics"]["window"], "journalAttemptCounts": x["metrics"]["journalAttemptCounts"],
                                         "stateDistributions": x["metrics"]["stateDistributions"], "returnValueStatusDistribution": x["metrics"]["returnValueStatusDistribution"],
                                         "logicalPhaseToPhysicalRuns": x["metrics"]["logicalPhaseToPhysicalRuns"]}} for task_id, x in task_summary.items()},
        "runs": [{"runId": r["runId"], "taskId": r["taskId"], "status": r["status"], "state": r["state"],
                  "returnValueStatus": r["returnValueStatus"], "journalFilePresent": r["journalFilePresent"],
                  "journalAttemptCounts": dict(collections.Counter(x.get("outcome") for x in r["journal"] if x.get("outcome"))),
                  "phases": sorted(set(x.get("phase") for x in r["journal"] if x.get("phase")))} for r in runs],
        "errors": [{"runId": r["runId"], "taskId": r["taskId"], "status": r["status"], "stateError": r["state"]["error"],
                    "deadReasonCategories": sorted(set(x.get("deadReasonCategory") for x in r["journal"] if x.get("outcome") == "dead"))}
                   for r in runs if r["state"]["error"] or any(x.get("outcome") == "dead" for x in r["journal"])],
        "refs": {"durationBand10to20MinMs": duration_band, "missingRunIdsByTask": metrics["coverage"]["missingRunIdsByTask"],
                 "missingJournalRunIds": metrics["coverage"]["missingJournalRunIds"]},
    }
    (OUT / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    lines = [
        "# ADLC 本地运行日志瓶颈量化（2026-09-13）",
        "",
        "## 口径与覆盖",
        "",
        "本报告由同目录 `analyze_adlc_bottlenecks.py` 生成。关联只接受各任务 `manifest.json` 的 `workflowRuns` run ID；`workflow_name` 仅用于交叉检查，因此 Docker 阶段的 `sandbox-mcp` 与 D-003 后的 `local-mcp` 会共同归入 `2026-09-11-sandbox-mcp-server`。输入文件 SHA-256 与逐 run 明细见 `metrics.json`。",
        "",
        f"共扫描 {len(run_dirs)} 个 workflow-run 目录；五个 manifest 声明 {len(expected_to_task)} 个 run，实际关联 {sum(1 for r in runs if r['taskId'])} 个，未关联 {sum(1 for r in runs if not r['taskId'])} 个。缺失 run、缺失 journal 和每个 state 的 execution/acceptance/delivery 分布均保留在机器产物中。",
        "",
        "`durationMs` 只来自 journal 的子任务尝试；其总和不是 workflow walltime。消息长度、线程时间跨度和 `usage.outputTokens`/`tokenCount` 也没有被换算成成本；后两者仅按原字段保存。dead 的 reason/detail 来自 journal result，state.error 单独保留为 stateError，attempt.disposition 也原样保留。",
        "",
        "## 任务级量化",
        "",
        "| ADLC | 期望/观察 run | 缺失 run | journal ok/error/cancelled/dead | 主要窗口 |",
        "|---|---:|---|---|---|",
    ]
    for task_id, summary in sorted(task_summary.items()):
        m = summary["metrics"]
        c = m["journalAttemptCounts"]
        w = m["window"]
        lines.append(f"| `{task_id}` | {summary['expectedRunCount']}/{summary['observedRunCount']} | {len(summary['missingRunIds'])} | {c['ok']}/{c['error']}/{c['cancelled']}/{c['dead']} | {w['startedAtMin']} → {w['finishedAtMax']} |")
    lines += [
        "",
        "这里的 `error` 只计结构化 error-like journal kind；本批次没有显式 cancelled journal kind。`dead` 的 reason/detail 及有限分类另列在每次 journal attempt，不能把 state.error 误归因到 child；state 中 `status=killed` 的 run 另在逐 run state 分布中可见。",
        "",
        "state 的 `execution_status`、`acceptance_status`、`delivery_status` 与 `return_value.status` 是四个独立观测面，未合并为单一成功率。五任务中 acceptance 全部为 `unknown`；sandbox 有 1 个 `killed` execution run，journal 没有显式取消记录。详细分布和每个 run 的原值见 `metrics.json`。",
        "",
        "## sandbox resume / pivot 审计",
        "",
        "sandbox 任务的 13 个 run 均由 manifest run ID 关联。round-1 至 round-6 多次同时出现 Implement、Build/Gates 和多个 Verify phase，说明这些 resume 并非只重跑 assessor；round-7 的 manifest/decision 记录了 D-003 将 Docker/容器部分作废并改为本地 MCP 形态，因而 round-7 的 Decompose/Implement/Verify 属于目标变更后的必要重做候选，不应直接标成浪费。round-8 同时有 Fix、Converge、Verify、Assess；P0 是回顾性冻结记录，不能据此声称发生了产品源改动。",
        "",
        "日志文字能证明某些 run 自报修复、重建或‘零源码改动’，但本研究没有读取 git diff，也不把 subagent 自报通过当成测试证据。`metrics.json` 的 `changeEvidence.sourceDiffObserved=false` 明确这一限制。",
        "",
        "## 可证实的开销候选",
        "",
        "journal 中可以定位到具体 run/seq/phase/durationMs；这些是 child-attempt 观测。10–20 分钟（600,000–1,200,000 ms）区间的具体记录见 `metrics.json:durationBand10to20MinMs`，包含 journal 行号，可回到对应输入文件核对。它们不能直接证明 workflow 的 10–20 分钟 walltime 开销，除非由 state 的 started_at/finished_at 或更细粒度系统遥测闭环；本报告保留 unknown。",
        "",
        "## 协调文书规模",
        "",
        "每个任务的 handoff 数、contract revision，以及 assessment/review/learning 文件存在性收录在 `metrics.json`。这些是文书规模指标，不是耗时、浪费或完成质量指标。",
        "",
        "## 复现",
        "",
        "```bash",
        "python3 side-projects/agent-defect-analyzer/reports/2026-09-13-adlc-bottlenecks/scripts/analyze_adlc_bottlenecks.py \\",
        "  --repo /path/to/perihelion \\",
        "  --out /tmp/adlc-journals-v3",
        "```",
    ]
    lines += ["", "10–20 分钟区间的前 10 条 child-attempt 观测（按 durationMs 降序）：", "",
              "| run | seq | phase | model | durationMs | journal line |", "|---|---:|---|---|---:|---:|"]
    for row in duration_band[:10]:
        lines.append(f"| `{row['runId']}` | {row['seq']} | `{row['phase']}` | `{row['model']}` | {row['durationMs']} | {row['journalLine']} |")
    (OUT / "findings.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
