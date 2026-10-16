#!/usr/bin/env bash
# changed-crate-args.sh — 把改动文件映射为 cargo -p 参数（可选反向依赖闭包）
#
# 背景：spec/history/2026-10.md 的 A3 收窄（pre-commit clippy 只跑改动 crate）与
# 交付前收窄 check（改动 crate + 反向依赖，见 scripts/check-changed.sh）共用同一套
# 「改动文件 → crate」映射，本脚本是该映射的单一权威。
#
# 映射由 `cargo metadata --no-deps` 的 manifest 目录派生，不再维护手写目录→package
# 清单：旧清单把目录名当 package 名，漏了 mcp-packages/workflow（peri-mcp-workflow），
# 且与 Cargo.lock 计数靠注释同步；目录名与 package 名分叉一处即静默漏检。
# `--no-deps` 只读 manifest，不需要 patch 配置、不触发网络、不编译（同 ci-test-inventory.sh）。
#
# 两种口径：
#   --direct-only  只输出改动直接命中的 crate（pre-commit clippy 用；上界保守、代价最低）
#   默认           扩张到反向依赖闭包（交付前收窄 check 用）；闭包覆盖全部 member 时
#                  输出 --workspace（避免把「等价全量」写成 26 个 -p）
#
# 边界规则：
#   a. 改动根 Cargo.toml / Cargo.lock → 输出 --workspace（依赖图可能整体变化，按 crate 收窄不安全）
#   b. 不属于任何 workspace member 的文件（docs/、scripts/、e2e/、side-projects/* 等；
#      side-projects 各自声明 [workspace]，是独立工作区）→ 不产生参数
#   c. 全部输入无 crate 命中 → 无参数输出（调用方据此跳过 cargo）
#
# 用法：
#   bash scripts/changed-crate-args.sh [--direct-only]
#   CHANGED_DIFF_FILES=$'peri-agent/src/lib.rs\nmcp-packages/cron/src/lib.rs' \
#     bash scripts/changed-crate-args.sh              # 测试注入，跳过 git
#
# 输入（按优先级）：
#   1. 环境变量 CHANGED_DIFF_FILES（只要「已设置」即采用，空串 = 空输入）
#   2. git diff --cached --name-only（暂存改动）
#   3. git diff --name-only HEAD（暂存区为空时回退到工作区改动）
#
# 输出：单行 cargo 参数（如 "-p peri-agent -p peri-tui" 或 "--workspace"）；无命中时无输出。
# 退出码：恒为 0。映射失败（metadata 不可用）时回退 --workspace 并 warn——
#   门控脚本的失败方向必须是「更严」，不能因为算不出收窄就静默跳过检查。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

direct_only=0
while [ $# -gt 0 ]; do
    case "$1" in
        --direct-only) direct_only=1 ;;
        -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
        *)
            echo "changed-crate-args.sh: 未知参数 $1" >&2
            exit 2
            ;;
    esac
    shift
done

files=""
if [ "${CHANGED_DIFF_FILES+set}" = "set" ]; then
    files="$CHANGED_DIFF_FILES"
else
    files="$(git diff --cached --name-only 2>/dev/null || true)"
    if [ -z "$files" ]; then
        files="$(git diff --name-only HEAD 2>/dev/null || true)"
    fi
fi

CHANGED_FILES="$files" DIRECT_ONLY="$direct_only" python3 - <<'PY'
import json
import os
import subprocess
import sys

root = os.getcwd()
direct_only = os.environ.get("DIRECT_ONLY") == "1"
files = [f.strip() for f in os.environ.get("CHANGED_FILES", "").splitlines() if f.strip()]

# 规则 a：根清单变更 → 全量
if any(f in ("Cargo.toml", "Cargo.lock") for f in files):
    print("--workspace")
    sys.exit(0)

try:
    out = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root, stderr=subprocess.PIPE
    )
    meta = json.loads(out)
except Exception as exc:  # 回退：宁可全量，不乱收窄
    print(f"[changed-crate-args] cargo metadata 不可用，回退 --workspace：{exc}", file=sys.stderr)
    print("--workspace")
    sys.exit(0)

members = {}
for pkg in meta["packages"]:
    rel = os.path.relpath(os.path.dirname(pkg["manifest_path"]), root)
    members[pkg["name"]] = "" if rel == "." else rel

# 最长目录前缀优先：peri-acp 不会命中 peri-acp-types/
ordered = sorted(((d, n) for n, d in members.items() if d), key=lambda kv: -len(kv[0]))


def owner_of(path):
    for directory, name in ordered:
        if path.startswith(directory + "/"):
            return name
    return None


direct = []
for path in files:
    name = owner_of(path)
    if name and name not in direct:
        direct.append(name)

if not direct:
    sys.exit(0)  # 规则 b/c：无 crate 命中 → 无输出

if not direct_only:
    reverse = {name: set() for name in members}
    for pkg in meta["packages"]:
        for dep in pkg["dependencies"]:
            if dep["name"] in members:
                reverse[dep["name"]].add(pkg["name"])
    seen = set(direct)
    stack = list(direct)
    while stack:
        for dependent in reverse.get(stack.pop(), ()):
            if dependent not in seen:
                seen.add(dependent)
                stack.append(dependent)
    direct = sorted(seen)
    if len(direct) == len(members):
        print("--workspace")
        sys.exit(0)

print(" ".join(f"-p {name}" for name in direct))
PY
