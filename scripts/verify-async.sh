#!/usr/bin/env bash
# verify-async.sh — 后台全量验证 + 结构化回执（与交互路径解耦）
#
# 为什么：交互路径（提交 hook、交付前收窄门）一旦编到 test target 就要付分钟级成本。
# 本地实测（2026-10-16，Apple Silicon，dev profile，根 target 热态）：
#   check --locked --workspace                无改动 6.96s；touch peri-model 后 24.85s
#   check --locked --workspace --all-targets  首次 192.43s；test target 热后 30.93s
# 本脚本自身实测（隔离 target/verify）：
#   冷建首轮 7m12s（check 306s + clippy 126s，一次性，target/verify 4.4G）；
#   稳态增量 50s（touch peri-model：check 23s + clippy 27s）
# 所以全量门改到隔离 target 上后台执行：交互路径只跑收窄门（scripts/check-changed.sh），
# 全量结论以回执文件为准，由下一轮会话读取，而不是把对话卡在编译上。
#
# 隔离 target：CARGO_TARGET_DIR=target/verify
#   - 与主 target 分开：不与其他 cargo 进程争文件锁（不会出现 `Blocking waiting for
#     file lock on build directory` 排队等待）
#   - 首次预热成本一次性支付；之后增量成本与主 target 同量级
#   - 清理：rm -rf target/verify target/verify-receipts target/verify.lock
#
# 单飞：同一 worktree 同时只跑一个后台验证（target/verify.lock + pid 存活检测）。
#   多个 worktree 各自持有独立 target 与锁，互不阻塞；机器级并发上限（多树并发时
#   load 会互相推高）留待实测数据需要时再加。
#
# 证据口径（对齐 TEST-EVIDENCE-001）：回执记录本次验证的快照 =
#   HEAD + `git diff HEAD` 哈希 + 未跟踪文件（路径与内容）哈希。`read` 会把回执快照与
#   当前工作区比对，不一致即判定 stale——stale 回执不能当作当前代码的证据。started
#   通知本身不算通过，判定只看每个 step 的 exit status。
#
# 命令范围（CI 同款入口的本地副本）：
#   1. check  --locked --workspace --all-targets
#   2. clippy --locked --workspace --all-targets -- -D warnings（check 失败时跳过）
#   cargo test --workspace 留在 CI：本地分钟级且吃 CPU，不进入回执承诺的 5 分钟窗口。
#
# 用法：
#   bash scripts/verify-async.sh start [--reason TEXT]   # 立即返回，后台继续
#   bash scripts/verify-async.sh read [--json]           # 汇报/开工前读最近回执
#   bash scripts/verify-async.sh status                  # 是否有验证在跑
# 退出码：start 0=已启动 / 3=已有验证在跑；read 0=已读到回执 / 4=还没有回执；status 恒 0。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
target_dir="$repo_root/target/verify"
receipts_dir="$repo_root/target/verify-receipts"
lock_dir="$repo_root/target/verify.lock"
cargo_script="$repo_root/scripts/cargo-rmcp-patched.sh"

now_utc() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# 工作区快照：HEAD + 已跟踪改动的 diff 哈希 + 未跟踪文件的路径与内容哈希
snapshot_json() {
    VT_ROOT="$repo_root" python3 - <<'PY'
import hashlib
import json
import os
import subprocess

root = os.environ["VT_ROOT"]


def git(*args, check=True):
    return subprocess.run(["git", *args], cwd=root, capture_output=True, check=check).stdout


head = git("rev-parse", "HEAD").decode().strip()
branch = git("rev-parse", "--abbrev-ref", "HEAD").decode().strip()
# 指纹只覆盖 Rust 验证的输入：子模块（peri-cool 文档站、e2e 等）不属于 cargo
# workspace，其 gitlink 提交与工作树由并发的其他会话改动，若计入会让回执在无关
# 噪声下反复被判 stale。子模块状态单独记录，仅作说明、不参与比对。
diff = git("diff", "HEAD", "--ignore-submodules=all")
# 未跟踪文件要连内容一起哈希：只哈希路径清单时，改一个未跟踪源文件的正文（路径不变）
# 不会让指纹变化，read 会把旧回执当成 fresh——回执就不再是当前代码的证据（TEST-EVIDENCE-001）。
untracked_paths = [p for p in git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0") if p]
uh = hashlib.sha256()
for path in sorted(untracked_paths):
    uh.update(path + b"\0")
    try:
        with open(os.path.join(root, path.decode()), "rb") as fh:
            while True:
                chunk = fh.read(1 << 16)
                if not chunk:
                    break
                uh.update(chunk)
    except OSError:
        uh.update(b"<unreadable>")  # 读不到也要让指纹变化，不能静默当作没改
untracked = uh.digest() if untracked_paths else b""
submodules = git("submodule", "status", check=False)


def short(blob):
    return hashlib.sha256(blob).hexdigest()[:16]


print(json.dumps({
    "head": head,
    "branch": branch,
    "diff_hash": short(diff),
    "untracked_hash": short(untracked),
    "submodules_hash": short(submodules),
    "dirty": bool(diff.strip()) or bool(untracked_paths),
}))
PY
}

lock_pid() { cat "$lock_dir/pid" 2>/dev/null || true; }

dir_mtime() {
    if stat -f %m "$1" >/dev/null 2>&1; then
        stat -f %m "$1"
    else
        stat -c %Y "$1"
    fi
}

# 锁存活判定：pid 存活即在跑；刚 mkdir、pid 尚未写入的短窗口（<120s）也视为在跑
lock_is_live() {
    [ -d "$lock_dir" ] || return 1
    local pid
    pid="$(lock_pid)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        return 0
    fi
    if [ -z "$pid" ]; then
        local age
        age=$(( $(date +%s) - $(dir_mtime "$lock_dir") ))
        [ "$age" -lt 120 ] && return 0
    fi
    return 1
}

cmd_start() {
    local reason="${1:-交付前门}"
    mkdir -p "$receipts_dir"
    if ! mkdir "$lock_dir" 2>/dev/null; then
        if lock_is_live; then
            echo "[verify-async] 已有后台验证在跑（pid $(lock_pid)，开始 $(cat "$lock_dir/started_at" 2>/dev/null || echo '?')）"
            echo "[verify-async] 读回执：bash scripts/verify-async.sh read"
            exit 3
        fi
        echo "[verify-async] 清理陈旧锁 ${lock_dir}（pid $(lock_pid) 已不存在）"
        rm -rf "$lock_dir"
        mkdir "$lock_dir"
    fi

    local snap stamp console
    snap="$(snapshot_json)"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    console="$receipts_dir/$stamp-console.log"

    nohup bash "$self" __run "$snap" "$reason" "$stamp" >"$console" 2>&1 &
    local child=$!
    printf '%s\n' "$child" >"$lock_dir/pid"
    printf '%s\n' "$(now_utc)" >"$lock_dir/started_at"
    printf '%s\n' "$snap" >"$lock_dir/snapshot.json"
    printf '%s\n' "$stamp" >"$lock_dir/stamp"
    printf '%s\n' "$reason" >"$lock_dir/reason"

    echo "[verify-async] 已启动 pid=${child}（隔离 target：${target_dir}）"
    echo "[verify-async] 回执目录：${receipts_dir}（本次 ${stamp}）"
    echo "[verify-async] 读取：bash scripts/verify-async.sh read"
}

cmd_run() {
    local snap="$1" reason="$2" stamp="$3"
    trap 'rm -rf "$lock_dir"' EXIT
    mkdir -p "$receipts_dir/logs"
    local base="$receipts_dir/logs/$stamp"
    local check_out="$base-check.out" clippy_out="$base-clippy.out"
    local check_exit=0 clippy_exit=0 clippy_skipped=0
    local check_secs=0 clippy_secs=0 t0 t1 verdict=pass
    local run_start started_at total_secs
    run_start=$(date +%s)
    started_at="$(cat "$lock_dir/started_at" 2>/dev/null || now_utc)"

    echo "[verify-async] run $stamp start=$(now_utc) reason=$reason"

    t0=$(date +%s)
    set +e
    CARGO_TARGET_DIR="$target_dir" "$cargo_script" check --locked --workspace --all-targets >"$check_out" 2>&1
    check_exit=$?
    set -e
    t1=$(date +%s)
    check_secs=$(( t1 - t0 ))
    echo "[verify-async] check exit=$check_exit ${check_secs}s"

    if [ "$check_exit" -eq 0 ]; then
        t0=$(date +%s)
        set +e
        CARGO_TARGET_DIR="$target_dir" "$cargo_script" clippy --locked --workspace --all-targets -- -D warnings >"$clippy_out" 2>&1
        clippy_exit=$?
        set -e
        t1=$(date +%s)
        clippy_secs=$(( t1 - t0 ))
        echo "[verify-async] clippy exit=$clippy_exit ${clippy_secs}s"
    else
        clippy_skipped=1
        : >"$clippy_out"
        echo "[verify-async] check 失败，跳过 clippy（回执标注 skipped）"
    fi

    [ "$check_exit" -ne 0 ] && verdict=fail
    if [ "$clippy_skipped" -eq 0 ] && [ "$clippy_exit" -ne 0 ]; then
        verdict=fail
    fi

    VT_ROOT="$repo_root" VT_STAMP="$stamp" VT_REASON="$reason" VT_SNAP="$snap" \
    VT_VERDICT="$verdict" VT_CHECK_EXIT="$check_exit" VT_CHECK_SECS="$check_secs" \
    VT_CLIPPY_EXIT="$clippy_exit" VT_CLIPPY_SECS="$clippy_secs" VT_CLIPPY_SKIPPED="$clippy_skipped" \
    VT_CHECK_OUT="$check_out" VT_CLIPPY_OUT="$clippy_out" VT_TARGET_DIR="$target_dir" \
    VT_STARTED_AT="$started_at" VT_TOTAL_SECS="$(( $(date +%s) - run_start ))" \
    python3 - <<'PY'
import json
import os
from datetime import datetime, timezone

root = os.environ["VT_ROOT"]
stamp = os.environ["VT_STAMP"]


def utc_now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def tail(path, lines=25, cap=4000):
    if not os.path.exists(path):
        return ""
    text = open(path, "r", errors="replace").read()
    text = "\n".join(text.splitlines()[-lines:])
    return text[-cap:]


steps = [{
    "name": "check",
    "cmd": "check --locked --workspace --all-targets",
    "target_dir": os.environ["VT_TARGET_DIR"],
    "exit": int(os.environ["VT_CHECK_EXIT"]),
    "seconds": int(os.environ["VT_CHECK_SECS"]),
    "status": "pass" if os.environ["VT_CHECK_EXIT"] == "0" else "fail",
    "tail": tail(os.environ["VT_CHECK_OUT"]),
}]
if os.environ["VT_CLIPPY_SKIPPED"] == "1":
    steps.append({
        "name": "clippy",
        "cmd": "clippy --locked --workspace --all-targets -- -D warnings",
        "exit": None,
        "seconds": 0,
        "status": "skipped",
        "reason": "check 失败，未运行 clippy",
        "tail": "",
    })
else:
    steps.append({
        "name": "clippy",
        "cmd": "clippy --locked --workspace --all-targets -- -D warnings",
        "target_dir": os.environ["VT_TARGET_DIR"],
        "exit": int(os.environ["VT_CLIPPY_EXIT"]),
        "seconds": int(os.environ["VT_CLIPPY_SECS"]),
        "status": "pass" if os.environ["VT_CLIPPY_EXIT"] == "0" else "fail",
        "tail": tail(os.environ["VT_CLIPPY_OUT"]),
    })

receipt = {
    "schema": 1,
    "worktree": root,
    "receipt_id": stamp,
    "trigger": os.environ["VT_REASON"],
    "snapshot": json.loads(os.environ["VT_SNAP"]),
    "started_at": os.environ["VT_STARTED_AT"],
    "finished_at": utc_now(),
    "seconds_total": int(os.environ["VT_TOTAL_SECS"]),
    "verdict": os.environ["VT_VERDICT"],
    "steps": steps,
    "not_run": ["cargo test --workspace（留在 CI）", "e2e/（见 docs/standards/testing.md）"],
    "logs": {
        "check": os.environ["VT_CHECK_OUT"],
        "clippy": os.environ["VT_CLIPPY_OUT"],
    },
}

receipts_dir = os.path.join(root, "target", "verify-receipts")
payload = json.dumps(receipt, ensure_ascii=False, indent=2)
for name in (f"{stamp}.json", "latest.json"):
    with open(os.path.join(receipts_dir, name), "w") as fh:
        fh.write(payload + "\n")

lines = [
    f"verdict: {receipt['verdict']}    trigger: {receipt['trigger']}",
    f"receipt_id: {receipt['receipt_id']}   total: {receipt['seconds_total']}s",
    f"snapshot: {receipt['snapshot']['head'][:12]} diff={receipt['snapshot']['diff_hash']} "
    f"dirty={receipt['snapshot']['dirty']}",
    f"started_at: {receipt['started_at']}   finished_at: {receipt['finished_at']}",
]
for step in steps:
    exit_text = "-" if step["exit"] is None else step["exit"]
    lines.append(f"  {step['name']:<7} status={step['status']:<7} exit={exit_text} seconds={step['seconds']}")
lines.append("not_run: " + "; ".join(receipt["not_run"]))
with open(os.path.join(receipts_dir, "latest.txt"), "w") as fh:
    fh.write("\n".join(lines) + "\n")

print("\n".join(lines))
PY

    echo "[verify-async] run $stamp done verdict=$verdict"
}

cmd_read() {
    local latest="$receipts_dir/latest.json"
    if [ ! -f "$latest" ]; then
        echo "[verify-async] 还没有回执（$latest 不存在）"
        echo "[verify-async] 启动：bash scripts/verify-async.sh start"
        exit 4
    fi
    local cur live=0
    cur="$(snapshot_json)"
    if lock_is_live; then
        live=1
    fi
    VR_LATEST="$latest" VR_CUR="$cur" VR_JSON="${1:-}" VR_LIVE="$live" \
    VR_LOCK_PID="$(lock_pid)" VR_RUNNING_SINCE="$(cat "$lock_dir/started_at" 2>/dev/null || true)" \
    python3 - <<'PY'
import json
import os

receipt = json.load(open(os.environ["VR_LATEST"]))
current = json.loads(os.environ["VR_CUR"])
snap = receipt["snapshot"]

same = all(snap.get(k) == current.get(k) for k in ("head", "diff_hash", "untracked_hash"))
submodules_drifted = snap.get("submodules_hash") != current.get("submodules_hash")
live = os.environ["VR_LIVE"] == "1"

if os.environ["VR_JSON"] == "--json":
    print(json.dumps({
        "receipt": receipt,
        "current_snapshot": current,
        "stale": not same,
        "submodules_drifted": submodules_drifted,
        "verification_running": live,
        "running_pid": os.environ.get("VR_LOCK_PID", ""),
    }, ensure_ascii=False, indent=2))
    raise SystemExit(0)

lines = [
    f"verdict: {receipt['verdict']}    trigger: {receipt.get('trigger', '')}",
    f"receipt_id: {receipt.get('receipt_id', '?')}   total: {receipt.get('seconds_total', '?')}s   "
    f"finished_at: {receipt.get('finished_at', '')}",
    f"snapshot_match: {'fresh（与当前工作区一致）' if same else 'stale（工作区已变，回执不代表当前代码）'}"
    f"   head: {snap['head'][:12]}",
]
for step in receipt.get("steps", []):
    exit_text = "-" if step.get("exit") is None else step["exit"]
    lines.append(f"  {step['name']:<7} status={step['status']:<7} exit={exit_text} seconds={step.get('seconds', 0)}")
lines.append("not_run: " + "; ".join(receipt.get("not_run", [])))
if submodules_drifted:
    lines.append("说明：子模块状态已变（不属于 cargo workspace，不计入快照比对）")
# 失败步骤的 tail 无条件打印：verdict=fail 且快照 fresh 才是要立刻处理的情形，
# 只在 stale 时打印会把「刚跑出来的失败」藏起来、反过来展示过期的失败。
for step in receipt.get("steps", []):
    if step["status"] == "fail" and step.get("tail"):
        lines.append(f"--- {step['name']} tail ---")
        lines.append(step["tail"])
if live:
    lines.append(f"注意：另有验证在跑（pid {os.environ.get('VR_LOCK_PID', '?')}，开始 {os.environ.get('VR_RUNNING_SINCE', '?')}）")
print("\n".join(lines))
PY
}

cmd_status() {
    if lock_is_live; then
        echo "[verify-async] running pid=$(lock_pid) started=$(cat "$lock_dir/started_at" 2>/dev/null || echo '?')"
        echo "[verify-async] 锁：${lock_dir}；回执：${receipts_dir}/latest.json"
        return 0
    fi
    if [ -f "$receipts_dir/latest.txt" ]; then
        echo "[verify-async] idle；最近回执："
        cat "$receipts_dir/latest.txt"
        echo "snapshot_match 请用 read 判定（需要比对当前工作区）"
    else
        echo "[verify-async] idle；还没有回执（bash scripts/verify-async.sh start 启动）"
    fi
}

case "${1:-}" in
    start)
        shift
        reason=""
        while [ $# -gt 0 ]; do
            case "$1" in
                --reason)
                    reason="${2:-}"
                    shift 2
                    ;;
                *)
                    echo "verify-async.sh: 未知参数 $1" >&2
                    exit 2
                    ;;
            esac
        done
        cmd_start "$reason"
        ;;
    __run)
        shift
        cmd_run "$@"
        ;;
    read)
        shift
        cmd_read "${1:-}"
        ;;
    status)
        cmd_status
        ;;
    *)
        sed -n '2,45p' "$self"
        exit 2
        ;;
esac
