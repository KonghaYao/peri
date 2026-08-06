#!/usr/bin/env bash
# Check that peri-resources/ doesn't import from any business crate.
#
# §0 依赖方向（docs/top-level.md）：Resources 是外部系统门面，只能依赖契约层
# （peri-acp-types）与标准/第三方依赖；import peri-agent / peri-acp / peri-tui /
# peri-middlewares / peri-model 等业务 crate 即越层（出现即契约下沉不彻底）。
#
# 白名单（资源实现，PRD 决策 20「既有 crate 归位」）：
# - peri-lsp 已接入（门面 src/lsp.rs，消费方经 Resources 使用）
# - peri-workflow 因依赖 peri-agent（→ peri-resources）成环暂未接入；接入时
#   一并加入白名单并在 Cargo.toml 注明
#
# 过渡边说明：TUI 启动处经 Resources::open() 直连本 crate（§0 图无
# TUI→Resources 边），属过渡态，随 M-TUI / L5（TUI 改经 ACP）收紧；
# 本脚本只校验 peri-resources 自身不向上 import，不校验消费方。

set -e
cd "$(dirname "$0")/.."

VIOLATIONS=$(grep -rn "use peri_\|use langfuse_client\|use agm::\|use acp_hub" \
    peri-resources/src/ \
    --include="*.rs" \
    | grep -v "use peri_acp_types::" \
    | grep -v "use peri_lsp::" \
    || true)

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Forbidden business-crate imports in peri-resources (only peri_acp_types / resource impls allowed):"
    echo "$VIOLATIONS"
    exit 1
fi

echo "✅ peri-resources imports OK (§0: contract layer peri-acp-types + resource impls)"
