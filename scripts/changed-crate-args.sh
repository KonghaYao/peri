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
# 两种改动来源：
#   --staged-only  只取暂存（git diff --cached）：提交门只对本次提交负责，不被工作区里
#                  未暂存的改动牵连
#   默认           暂存 + 未暂存 + 未跟踪（git diff HEAD 与 git ls-files --others 的并集）：
#                  交付前门看的是当前工作区状态。只取暂存会漏掉「A 已暂存、B 未暂存」的
#                  另一半；只取 git diff HEAD 会漏掉新建、尚未 add 的源码文件
#
# 边界规则：
#   a. 构建输入变更 → 输出 --workspace：根 Cargo.toml / Cargo.lock（依赖图可能整体变化），
#      以及 patches/、.cargo/（前者由 scripts/cargo-rmcp-patched.sh 注入依赖源码，后者改
#      rustflags）。它们不进 crate 映射，但会改变整体构建结果，按 crate 收窄不安全
#   b. 不属于任何 workspace member 的文件（docs/、scripts/、e2e/、side-projects/* 等；
#      side-projects 各自声明 [workspace]，是独立工作区）→ 不产生参数
#   c. 全部输入无 crate 命中 → 无参数输出（调用方据此跳过 cargo）
#
# 用法：
#   bash scripts/changed-crate-args.sh [--direct-only] [--staged-only]
#   CHANGED_DIFF_FILES=$'peri-agent/src/lib.rs\nmcp-packages/cron/src/lib.rs' \
#     bash scripts/changed-crate-args.sh              # 测试注入，跳过 git
#
# 输入（按优先级）：
#   1. 环境变量 CHANGED_DIFF_FILES（只要「已设置」即采用，空串 = 空输入）
#   2. --staged-only：git diff --cached --name-only
#   3. 默认：git diff --name-only HEAD 与 git ls-files --others --exclude-standard 的并集
#
# 输出：单行 cargo 参数（如 "-p peri-agent -p peri-tui" 或 "--workspace"）；无命中时无输出。
# 退出码：恒为 0。映射失败（metadata 不可用）时回退 --workspace 并 warn——
#   门控脚本的失败方向必须是「更严」，不能因为算不出收窄就静默跳过检查。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

direct_only=0
staged_only=0
while [ $# -gt 0 ]; do
    case "$1" in
        --direct-only) direct_only=1 ;;
        --staged-only) staged_only=1 ;;
        -h|--help) sed -n '2,46p' "$0"; exit 0 ;;
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
elif [ "$staged_only" = "1" ]; then
    files="$(git diff --cached --name-only 2>/dev/null || true)"
else
    nl=$'\n'
    files="$(git diff --name-only HEAD 2>/dev/null || true)"
    untracked="$(git ls-files --others --exclude-standard 2>/dev/null || true)"
    if [ -n "$untracked" ]; then
        files="${files:+$files$nl}$untracked"
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

# 规则 a：构建输入变更 → 全量。patches/ 与 .cargo/ 按目录整体从宽——判到具体文件
# 会再引入一套「哪个补丁文件真的进构建」的知识，漏一处又是同一种静默收窄。
BUILD_INPUT_FILES = ("Cargo.toml", "Cargo.lock")
BUILD_INPUT_DIRS = ("patches/", ".cargo/")

if any(f in BUILD_INPUT_FILES or f.startswith(BUILD_INPUT_DIRS) for f in files):
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
