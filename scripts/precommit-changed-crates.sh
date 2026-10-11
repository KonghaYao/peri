#!/usr/bin/env bash
# precommit-changed-crates.sh — pre-commit clippy 收窄：把改动文件映射为 cargo -p 参数
#
# 背景：spec/history/2026-10.md（2026-10-05 条目，原 §四 A3）。
# pre-commit 原先每次提交对全 workspace 跑 clippy，缓存冷时成本接近一次全量；
# 全量 clippy 已由 CI（.github/workflows/ci.yml）覆盖，pre-commit 只需覆盖
# 「本次改动直接涉及的 crate」。check 保持全量（类型门不放松）。
#
# 用法：
#   bash scripts/precommit-changed-crates.sh
#   PRECOMMIT_DIFF_FILES=$'peri-agent/src/lib.rs\nmcp-packages/cron/src/lib.rs' \
#     bash scripts/precommit-changed-crates.sh     # 测试注入，跳过 git
#
# 输入（按优先级）：
#   1. 环境变量 PRECOMMIT_DIFF_FILES（只要「已设置」即采用，空串 = 空输入）
#   2. git diff --cached --name-only（暂存改动）
#   3. git diff --name-only HEAD（暂存区为空时回退到工作区改动）
#
# 输出：单行 clippy 参数（如 "-p peri-agent -p peri-mcp-web"）；无命中时无输出。
#
# 边界规则：
#   a. 改动根 Cargo.toml 或根 Cargo.lock → 输出全部 workspace members
#      （依赖图可能整体变化，按 crate 收窄不安全，保守回退全量）；
#   b. 改动不属于任何 member 的文件（docs/、scripts/、npm-packages/、
#      e2e/、side-projects/* 等；side-projects 各自声明 [workspace]，是独立
#      工作区，根 cargo 无法 -p 选中）→ 不产生参数；
#   c. 全部输入无 crate 命中 → 无参数输出（调用方据此跳过 cargo）。
#
# workspace 成员口径：根 Cargo.toml members（显式 24 个）+ peri-theme
# （peri-tui 的 path 依赖，自动入组），共 25 个；已与根 Cargo.lock 的本地包
# 清单（无 source 的 package）逐一核对一致。诊断报告里的「26 个 crate」是
# 写作时旧计数（peri-mcp-lsp 已删除）。
#
# 已知代价：改动底层 crate 后，上层依赖 crate 新引入的 clippy 警告不会再在
# pre-commit 暴露，由 CI 全量 clippy 兜底。
#
# 退出码：恒为 0（映射本身失败不应阻塞提交；调用方以「无输出」判定跳过）。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# 路径前缀 → package 名。prefix 为仓库根相对目录；逐一核对过各目录 Cargo.toml
# 的 [package].name——mcp-packages/* 的 package 名与目录名不同
# （common → peri-mcp-common 等），不能靠目录名推导。
# 匹配要求 '/' 边界，因此 "peri-acp" 不会误命中 "peri-acp-types/..."。
MAPPING=(
    "langfuse-client=langfuse-client"
    "mcp-packages/artifact=peri-mcp-artifact"
    "mcp-packages/common=peri-mcp-common"
    "mcp-packages/config=peri-mcp-config"
    "mcp-packages/credentials=peri-mcp-credentials"
    "mcp-packages/cron=peri-mcp-cron"
    "mcp-packages/web=peri-mcp-web"
    "mcp-packages/workspace=peri-mcp-workspace"
    "peri-acp=peri-acp"
    "peri-acp-types=peri-acp-types"
    "peri-agent=peri-agent"
    "peri-config=peri-config"
    "peri-controller=peri-controller"
    "peri-js-runtime=peri-js-runtime"
    "peri-mcp-core=peri-mcp-core"
    "peri-middlewares=peri-middlewares"
    "peri-model=peri-model"
    "peri-process=peri-process"
    "peri-resources=peri-resources"
    "peri-runtime=peri-runtime"
    "peri-theme=peri-theme"
    "peri-time=peri-time"
    "peri-tui=peri-tui"
    "peri-wasm=peri-wasm"
    "peri-workflow=peri-workflow"
)

selected=""

# 去重追加（bash 3.2 无关联数组，用空格分隔集合）
add_pkg() {
    case " $selected " in
        *" $1 "*) ;;
        *) selected="${selected:+$selected }$1" ;;
    esac
}

files=""
if [ "${PRECOMMIT_DIFF_FILES+set}" = "set" ]; then
    files="$PRECOMMIT_DIFF_FILES"
else
    files="$(git diff --cached --name-only 2>/dev/null || true)"
    if [ -z "$files" ]; then
        files="$(git diff --name-only HEAD 2>/dev/null || true)"
    fi
fi

need_all=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    case "$file" in
        Cargo.toml|Cargo.lock)
            # 规则 a：根清单变更 → 回退全量
            need_all=1
            continue
            ;;
    esac
    for entry in "${MAPPING[@]}"; do
        prefix="${entry%%=*}"
        case "$file" in
            "$prefix"/*)
                add_pkg "${entry#*=}"
                break
                ;;
        esac
    done
done < <(printf '%s\n' "$files")

if [ "$need_all" -eq 1 ]; then
    for entry in "${MAPPING[@]}"; do
        add_pkg "${entry#*=}"
    done
fi

args=""
for pkg in $selected; do
    args="${args:+$args }-p $pkg"
done

if [ -n "$args" ]; then
    printf '%s\n' "$args"
fi
