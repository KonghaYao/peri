#!/usr/bin/env bash
# check-changed.sh — 交付前收窄类型门
#
# 只对「本次改动直接涉及的 crate + 依赖它们的 crate（反向依赖闭包）」跑 cargo check
# （默认 targets：lib+bins），把分钟级成本移出交互路径。实测（2026-10-16，根 target
# 热态）：全量 check 无改动 6.96s、touch peri-model 后 24.85s；而 check --all-targets
# 首次要为 test target 付 192.43s。收窄对底层 crate 收益有限（peri-model 的反向依赖
# 闭包覆盖 26 个 member 中的 19 个），收益主要在上层 crate 与「不编 test target」。
#
# 全量门不在这里：`--workspace --all-targets` 与 CI 同款 clippy 由后台验证覆盖
#   bash scripts/verify-async.sh start    # 异步跑，结论写结构化回执
#   bash scripts/verify-async.sh read     # 开工/汇报前读回执
# CI（.github/workflows/ci.yml）跑同样范围的全量 build、测试与 clippy，是最终事实源。
#
# 用法：
#   bash scripts/check-changed.sh
#   CHANGED_DIFF_FILES=$'peri-agent/src/lib.rs' bash scripts/check-changed.sh   # 测试注入
#
# 无 crate 命中（只改文档/脚本等）时不调用 cargo，直接通过并说明。
# 退出码：cargo check 的退出码；跳过时为 0。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

args="$(bash "$repo_root/scripts/changed-crate-args.sh")"
if [ -z "$args" ]; then
    echo "[check-changed] 改动未命中任何 workspace crate，跳过 cargo check"
    echo "[check-changed] 全量门：bash scripts/verify-async.sh start（后台）+ CI"
    exit 0
fi

echo "[check-changed] cargo check --locked $args"
# shellcheck disable=SC2086 — args 是脚本产出的 -p 列表或 --workspace，需要分词
exec "$repo_root/scripts/cargo-rmcp-patched.sh" check --locked $args
