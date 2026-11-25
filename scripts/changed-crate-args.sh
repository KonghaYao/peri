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
#   a. 共享构建输入变更 → 输出 --workspace：根 manifest/lock、工具链、Cargo 配置、
#      补丁和 Cargo 构建包装脚本均可能改变 workspace 所有 crate 的结果，不按 crate 收窄
#   b. 不属于任何 workspace member 的文件（docs/、scripts/、side-projects/* 等；
#      side-projects 各自声明 [workspace]，是独立工作区）→ 不产生参数
#   c. 全部输入无 crate 命中 → 无参数输出（调用方据此跳过 cargo）
#
# 用法：
#   bash scripts/changed-crate-args.sh [--direct-only] [--staged-only]
#   bash scripts/changed-crate-args.sh --ci-pr BASE_SHA
#   CI compares the checked-out merge commit to its base parent. Only ordinary
#   Rust source/test edits and verified package assets are eligible; unknown
#   inputs fall back to --workspace.
#   CHANGED_DIFF_FILES=$'peri-agent/src/lib.rs\nmcp-packages/cron/src/lib.rs' \
#     bash scripts/changed-crate-args.sh              # 测试注入，跳过 git
#
# 输入（按优先级）：
#   1. 环境变量 CHANGED_DIFF_FILES（只要「已设置」即采用，空串 = 空输入）
#   2. --staged-only：git diff --cached --name-only
#   3. 默认：git diff --name-only HEAD 与 git ls-files --others --exclude-standard 的并集
#
# 输出：单行 cargo 参数（如 "-p peri-agent -p peri-tui" 或 "--workspace"）；无命中时无输出。
# Git 路径读取或 metadata 不可用时回退 --workspace 并 warn——
#   门控脚本的失败方向必须是「更严」，不能因为算不出收窄就静默跳过检查。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

direct_only=0
staged_only=0
ci_pr=0
ci_base=""
while [ $# -gt 0 ]; do
    case "$1" in
        --direct-only) direct_only=1 ;;
        --staged-only) staged_only=1 ;;
        --ci-pr) ci_pr=1; ci_base="${2:?--ci-pr requires a base SHA}"; shift ;;
        -h|--help) sed -n '2,46p' "$0"; exit 0 ;;
        *)
            echo "changed-crate-args.sh: 未知参数 $1" >&2
            exit 2
            ;;
    esac
    shift
done

DIRECT_ONLY="$direct_only" STAGED_ONLY="$staged_only" CI_PR="$ci_pr" CI_BASE="$ci_base" python3 - <<'PYTHON'
import json
import os
import re
from pathlib import Path
import subprocess
import sys

root = os.getcwd()
direct_only = os.environ.get("DIRECT_ONLY") == "1"
staged_only = os.environ.get("STAGED_ONLY") == "1"
ci_pr = os.environ["CI_PR"] == "1"


def git_paths(*args):
    data = subprocess.check_output(
        ["git", *args], cwd=root, stderr=subprocess.PIPE
    )
    return [os.fsdecode(path) for path in data.split(b"\0") if path]


if ci_pr:
    try:
        doc_rules = Path("scripts/ci-docs-only-patterns.txt").read_text().splitlines()
        docs = [re.compile(rule) for rule in doc_rules if rule]
        parent = subprocess.check_output(
            ["git", "rev-parse", "--verify", "HEAD^1"],
            cwd=root, text=True, stderr=subprocess.PIPE,
        ).strip()
        subprocess.run(
            ["git", "rev-parse", "--verify", "HEAD^2"], cwd=root, check=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        if parent != os.environ["CI_BASE"] or direct_only or staged_only:
            raise ValueError("PR merge base parent or selection mode does not match")
        changed = git_paths("diff", "--no-renames", "--name-only", "-z", parent, "HEAD", "--")
        files = [path for path in changed if not any(rule.search(path) for rule in docs)]
    except (OSError, ValueError, re.error, subprocess.CalledProcessError) as exc:
        print(f"[changed-crate-args] full workspace: {exc}", file=sys.stderr)
        print("--workspace")
        sys.exit(0)
elif "CHANGED_DIFF_FILES" in os.environ:
    # 测试入口按换行分隔，但保留每个路径的空白。
    files = [path for path in os.environ["CHANGED_DIFF_FILES"].split("\n") if path]
else:
    # --no-renames 将重命名拆成删除与新增：移出 crate 或共享构建输入也必须检查旧侧。
    # -z 让 Git 不给非 ASCII/换行等文件名加引号或转义。
    try:
        if staged_only:
            files = git_paths("diff", "--cached", "--name-only", "-z", "--no-renames")
        else:
            files = git_paths("diff", "--name-only", "-z", "--no-renames", "HEAD")
            files += git_paths("ls-files", "--others", "--exclude-standard", "-z")
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"[changed-crate-args] 无法读取 Git 路径，回退 --workspace：{exc}", file=sys.stderr)
        print("--workspace")
        sys.exit(0)

# 规则 a：构建输入变更 → 全量。patches/ 与 .cargo/ 按目录整体从宽——判到具体文件
# 会再引入一套「哪个补丁文件真的进构建」的知识，漏一处又是同一种静默收窄。
BUILD_INPUT_FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "Cross.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    "scripts/cargo-rmcp-patched.sh",
)
BUILD_INPUT_DIRS = ("patches/", ".cargo/")

if any(f in BUILD_INPUT_FILES or f.startswith(BUILD_INPUT_DIRS) for f in files):
    print("--workspace")
    sys.exit(0)

if not files:
    if ci_pr:
        print("--workspace")
    sys.exit(0)

# The terminal tests in peri-mcp-workspace import this file with #[path].
# Cargo metadata therefore has no dependency edge for this shared test input.
if ci_pr and "peri-middlewares/src/at_mention/work_fixture.rs" in files:
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
    rel = Path(os.path.relpath(Path(pkg["manifest_path"]).parent, root)).as_posix()
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
    if ci_pr:
        relative = path[len(members[name]) + 1:] if name else ""
        rust_source = relative.startswith(("src/", "tests/")) and relative.endswith(".rs")
        tui_locale = name == "peri-tui" and relative in {
            "locales/en/main.ftl", "locales/zh-CN/main.ftl",
        }
        if not (rust_source or relative == "README.md" or tui_locale):
            print("--workspace")
            sys.exit(0)
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
    # Keep all four Compact gates for Agent changes. A wasm-only change, or a
    # future standalone crate with no known native tests, also uses the full job.
    native_suites = {"peri-tui", "peri-swarm", "peri-browser-gateway"}
    if ci_pr and ("peri-agent" in seen or seen.isdisjoint(native_suites)):
        print("--workspace")
        sys.exit(0)
    if len(direct) == len(members):
        print("--workspace")
        sys.exit(0)

print(" ".join(f"-p {name}" for name in direct))
PYTHON
