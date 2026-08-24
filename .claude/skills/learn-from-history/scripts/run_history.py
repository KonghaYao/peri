#!/usr/bin/env python3
"""为 learn-from-history 创建隔离、可审计的一次性分析运行。"""

import argparse
import hashlib
import json
import os
import secrets
import shutil
import sqlite3
import sys
from datetime import date, datetime, timezone
from pathlib import Path

from extract_daily import extract_date_by_thread, get_db_path, normalize_cwd, query_active_days

MANIFEST_VERSION = 1
DEFAULT_RUN_ROOT = "/tmp/learn-from-history"
DEFAULT_MAX_UNITS = 7
DEFAULT_TARGET_KB = 250
DEFAULT_MAX_THREADS = 15


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_private_json(path, payload):
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    os.chmod(path, 0o600)


def write_private_text(path, content):
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(content)
    os.chmod(path, 0o600)


def snapshot_database(source_path, snapshot_path):
    source = sqlite3.connect(f"file:{source_path}?mode=ro", uri=True)
    target = sqlite3.connect(snapshot_path)
    try:
        source.backup(target)
    finally:
        target.close()
        source.close()
    os.chmod(snapshot_path, 0o600)


def make_run_dir(root):
    now = datetime.now(timezone.utc)
    run_id = f"{now.strftime('%Y%m%dT%H%M%SZ')}-{os.getpid()}-{secrets.token_hex(3)}"
    root = Path(root).expanduser().resolve()
    root_existed = root.exists()
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not root_existed or root == Path(DEFAULT_RUN_ROOT).resolve():
        os.chmod(root, 0o700)
    run_dir = root / run_id
    run_dir.mkdir(mode=0o700)
    os.chmod(run_dir, 0o700)
    return run_id, run_dir, now


def plan_units(items, target_kb, max_threads, max_units):
    """先按工作量分片；单元过多时反复合并最小单元。"""
    units = []
    current = []
    current_kb = 0.0
    for item in sorted(items, key=lambda value: (value["day"], value["path"])):
        would_overflow = current and (
            current_kb + item["size_kb"] > target_kb
            or len(current) >= max_threads
        )
        if would_overflow:
            units.append(current)
            current = []
            current_kb = 0.0
        current.append(item)
        current_kb += item["size_kb"]
    if current:
        units.append(current)

    while len(units) > max_units:
        smallest = sorted(range(len(units)), key=lambda index: (sum(item["size_kb"] for item in units[index]), index))[:2]
        left, right = sorted(smallest)
        units[left] = sorted(units[left] + units[right], key=lambda value: (value["day"], value["path"]))
        del units[right]
    return units


def unit_prompt(run_dir, unit):
    input_lines = "\n".join(f"- `{run_dir / item['path']}`" for item in unit["inputs"])
    return f"""你已经是负责 `{unit['id']}` 的 general-purpose 执行子 agent。不要调用 Agent 或派发子 agent；不要修改仓库。

逐个完整读取以下 thread 提取文件：
{input_lines}

任务：
1. 分析每个 thread 的用户意图、结果、真实异常、用户纠正和成功模式。
2. 只把可证伪的改进写为 finding；区分规则缺口、active issue 已覆盖、skill 缺陷、仅执行偏差和外部阻塞。
3. 写人类报告到 `{run_dir / unit['summary_path']}`。
4. 写机器 sidecar 到 `{run_dir / unit['sidecar_path']}`，格式严格为：

```json
{{
  "unit_id": "{unit['id']}",
  "status": "analyzed",
  "input_files": [
    {{"path": "相对 run 目录的路径", "sha256": "manifest 中的值", "status": "analyzed", "notes": ""}}
  ],
  "thread_count": {unit['expected_thread_count']},
  "message_count": {unit['expected_message_count']},
  "findings": [
    {{
      "id": "F-001",
      "classification": "rule_gap|active_issue_covered|skill_gap|execution_deviation|external_blocker",
      "evidence": ["thread id + 可核对事件"],
      "counterevidence": [],
      "frequency": "带分母的频次",
      "impact": "high|medium|low",
      "confidence": "high|medium|low",
      "fact_source": "建议事实源或 existing path",
      "acceptance": "如何证明改进有效"
    }}
  ],
  "blocked": [],
  "degraded_inputs_reviewed": []
}}
```

完成标准：`input_files` 必须与 manifest 中本单元文件集合完全相等；逐项复制 manifest digest。任何文件未读时将其 status 设为 `blocked` 并说明原因，不得自称完成。若输入含截断或解析失败，人工复核后把对应相对路径加入 `degraded_inputs_reviewed`，否则保持 blocked。写完后将 summary 与 sidecar 权限收紧为 `0600`，不得输出凭据、认证头或未脱敏的私密配置。
"""


def build_manifest(args):
    source_db = Path(args.db).expanduser().resolve()
    if not source_db.exists():
        raise FileNotFoundError(f"数据库文件不存在: {source_db}")
    if args.days < 1:
        raise ValueError("days 必须大于 0")
    if args.max_units < 1 or args.target_kb < 1 or args.max_threads < 1:
        raise ValueError("max-units、target-kb 与 max-threads 必须大于 0")

    run_id, run_dir, created_at = make_run_dir(args.run_root)
    try:
        snapshot_dir = run_dir / "snapshot"
        snapshot_dir.mkdir(mode=0o700)
        snapshot_path = snapshot_dir / "threads.db"
        snapshot_database(source_db, snapshot_path)

        cwd = None if args.all else normalize_cwd(args.cwd or os.getcwd())
        today = date.fromisoformat(args.today) if args.today else date.today()
        rows = query_active_days(str(snapshot_path), days=args.days, cwd=cwd, today=today)
        active_days = sorted(row["day"] for row in rows)
    except Exception:
        shutil.rmtree(run_dir, ignore_errors=True)
        raise

    manifest = {
        "version": MANIFEST_VERSION,
        "run_id": run_id,
        "status": "extracting",
        "created_at": created_at.isoformat(),
        "run_dir": str(run_dir),
        "project_filter": cwd,
        "all_projects": args.all,
        "window": {"days": args.days, "today": today.isoformat(), "active_days": active_days},
        "snapshot": {
            "path": str(snapshot_path.relative_to(run_dir)),
            "sha256": sha256_file(snapshot_path),
            "source_mtime_ns": source_db.stat().st_mtime_ns,
            "source_size": source_db.stat().st_size,
        },
        "limits": {"max_units": args.max_units, "target_kb": args.target_kb, "max_threads": args.max_threads},
        "days": [],
        "units": [],
        "totals": {"thread_count": 0, "message_count": 0, "truncations": 0, "parse_failures": 0},
        "failures": [],
    }
    manifest_path = run_dir / "manifest.json"
    write_private_json(manifest_path, manifest)

    items = []
    for day in active_days:
        out_dir = run_dir / "extracted" / day
        try:
            count, results = extract_date_by_thread(day, str(snapshot_path), str(out_dir), cwd=cwd)
            day_record = {"day": day, "status": "passed", "thread_count": count, "files": []}
            for result in results.values():
                path = Path(result["path"])
                relative_path = str(path.relative_to(run_dir))
                item = {
                    "day": day,
                    "path": relative_path,
                    "sha256": sha256_file(path),
                    "thread_id": result["thread_id"],
                    "message_count": result["msgs"],
                    "errors": result["errors"],
                    "truncations": result["truncations"],
                    "parse_failures": result["parse_failures"],
                    "size_kb": result["size_kb"],
                }
                day_record["files"].append(item)
                items.append(item)
                manifest["totals"]["message_count"] += result["msgs"]
                manifest["totals"]["truncations"] += result["truncations"]
                manifest["totals"]["parse_failures"] += result["parse_failures"]
            manifest["totals"]["thread_count"] += count
            manifest["days"].append(day_record)
        except Exception as error:
            manifest["failures"].append({"day": day, "error_type": type(error).__name__})
            manifest["days"].append({"day": day, "status": "failed", "thread_count": 0, "files": []})

    if items:
        summaries_dir = run_dir / "summaries"
        summaries_dir.mkdir(mode=0o700)

    for index, unit_items in enumerate(plan_units(items, args.target_kb, args.max_threads, args.max_units), start=1):
        unit_id = f"unit-{index:03d}"
        unit = {
            "id": unit_id,
            "expected_status": "analyzed",
            "dates": sorted({item["day"] for item in unit_items}),
            "inputs": unit_items,
            "expected_thread_count": len(unit_items),
            "expected_message_count": sum(item["message_count"] for item in unit_items),
            "expected_truncations": sum(item["truncations"] for item in unit_items),
            "expected_parse_failures": sum(item["parse_failures"] for item in unit_items),
            "summary_path": f"summaries/{unit_id}.md",
            "sidecar_path": f"summaries/{unit_id}.json",
            "prompt_path": f"prompts/{unit_id}.txt",
        }
        manifest["units"].append(unit)
        write_private_text(run_dir / unit["prompt_path"], unit_prompt(run_dir, unit))

    manifest["status"] = "failed" if manifest["failures"] else ("empty" if not items else "ready")
    write_private_json(manifest_path, manifest)
    return run_dir, manifest


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description="创建 learn-from-history snapshot run 与分析 manifest")
    parser.add_argument("--db", default=get_db_path(), help="SQLite 数据库路径")
    parser.add_argument("--days", type=int, default=7, help="含今天在内的自然日期数量")
    project_group = parser.add_mutually_exclusive_group()
    project_group.add_argument("--cwd", default=None, help="项目根目录；默认当前目录")
    project_group.add_argument("--all", action="store_true", help="不过滤项目")
    parser.add_argument("--run-root", default=DEFAULT_RUN_ROOT, help="隔离 run 根目录")
    parser.add_argument("--max-units", type=int, default=DEFAULT_MAX_UNITS)
    parser.add_argument("--target-kb", type=int, default=DEFAULT_TARGET_KB)
    parser.add_argument("--max-threads", type=int, default=DEFAULT_MAX_THREADS)
    parser.add_argument("--today", default=None, help=argparse.SUPPRESS)
    return parser.parse_args(argv)


def main():
    try:
        run_dir, manifest = build_manifest(parse_args())
    except (FileNotFoundError, ValueError, sqlite3.Error, OSError) as error:
        print(f"错误: {error}", file=sys.stderr)
        return 1

    print(f"Run: {run_dir}")
    print(f"Status: {manifest['status']}")
    print(f"Snapshot days: {len(manifest['window']['active_days'])}")
    print(f"Threads: {manifest['totals']['thread_count']}")
    print(f"Messages: {manifest['totals']['message_count']}")
    print(f"Analysis units: {len(manifest['units'])}")
    print(f"Manifest: {run_dir / 'manifest.json'}")
    if manifest["failures"]:
        print(f"Failed dates: {len(manifest['failures'])}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
