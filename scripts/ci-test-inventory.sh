#!/usr/bin/env bash
# CI 测试清单：枚举 workspace 的测试目标，按 CI 实际执行范围分类，并核对清单
# 与 .github/workflows/ci.yml 的过滤器一致。
#
# 目的有三个：
#   1. 给出「实际执行的目标」与「只构建未执行的目标」的可核对清单（迁移前后比较
#      测试身份用，不是只看总数）；
#   2. `--check` 守住必跑套件：CI 依赖的测试二进制一旦被改名/删除，这里先失败，
#      而不是等某个步骤静默命中零例；
#   3. `--check` 守住清单与 ci.yml 的一致性：解析每个 `-E` 过滤器表达式
#      （package/binary/kind、not/!、and/&/-、or/|/+、括号），按真实选择结果判定
#      executed；只抽取名字集合会把 `package(=peri-acp) & kind(=lib)` 误当成执行了
#      全部 ACP 目标，正是这里要避免的。语义解析与核对在
#      scripts/ci_test_inventory.py，其单测见 scripts/test-ci-test-inventory.py
#      （fixture 驱动，不需要 cargo）。
#
# 用 `cargo metadata --no-deps`：只读 manifest，不解析依赖，因此不需要 patch 配置、
# 不触发网络、也不编译。
#
# 「某步骤命中零例」由 ci.yml 兜底：每个 nextest 步骤都带 `--no-tests=fail`
# （nextest 0.9.85 起为默认值），命中零例时以退出码 4 失败。Compact 四套对抗
# 必须逐 binary 单独调用，合并成 union 时该开关只约束合并结果。
#
# 用法: ci-test-inventory.sh [--check] [--json]
#   --check  必跑套件缺失、过滤器语义与清单脱节时 exit 1
#   --json   输出机器可读清单（配合 --check 时结论仍然只走 stderr）

set -euo pipefail

check=false
as_json=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --check) check=true; shift ;;
        --json) as_json=true; shift ;;
        -h|--help)
            awk 'NR > 1 && /^set -euo pipefail/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "usage: $0 [--check] [--json]" >&2
            exit 2
            ;;
    esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/ci.yml"

# 执行范围的事实源：与 ci.yml 各测试步骤的 `-E` 一一对应。改任一侧都会让
# `--check` 失败，因此两边必须同时更新。
#
# 第一个步骤是「workspace 减去下面 10 个包」，其余步骤按包执行全部目标；
# Compact 四套对抗额外用 binary 具名，保证逐套命中非零用例。
# 两者合起来构成全部执行范围。
workspace_excluded=(
    peri-acp
    peri-agent
    peri-tui
    peri-middlewares
    peri-mcp-common
    peri-mcp-web
    peri-mcp-artifact
    peri-mcp-cron
    peri-mcp-monitor
    peri-mcp-workspace
)

# 其余步骤的期望执行范围，形如 `<package>:<target-kind>:<target-name>`；
# kind 或 name 为 `*` 表示该 package 的全部测试目标。这里声明的是期望集合，
# 实际 executed 由 ci.yml 的过滤器语义计算后与之比对。
step_includes=(
    'peri-acp:*:*'
    'peri-agent:*:*'
    'peri-agent:test:compact_pressure_adversarial_test'
    'peri-agent:test:compact_session_adversarial_test'
    'peri-agent:test:compact_session_manual_adversarial_test'
    'peri-agent:test:compact_authority_projection_test'
    'peri-tui:*:*'
    'peri-middlewares:*:*'
    'peri-mcp-common:*:*'
    'peri-mcp-web:*:*'
    'peri-mcp-artifact:*:*'
    'peri-mcp-cron:*:*'
    'peri-mcp-monitor:*:*'
    'peri-mcp-workspace:*:*'
)

# doctest 步骤的范围：`cargo test --doc --workspace --exclude ...`。
# nextest 不支持 doctest，因此单独一个入口，与原 `cargo test` 的范围保持一致。
doctest_excluded=(
    peri-acp
    peri-tui
    peri-middlewares
)

# 必跑套件（--check 的判据）。ARC-COMPACT-001 要求 Compact 对抗套件真实执行
# 且各自命中非零用例，因此它们单列而不是依赖通配。
required_suites=(
    'peri-agent:test:compact_pressure_adversarial_test'
    'peri-agent:test:compact_session_adversarial_test'
    'peri-agent:test:compact_session_manual_adversarial_test'
    'peri-agent:test:compact_authority_projection_test'
    'peri-agent:test:middleware_tests'
    'peri-agent:test:compact_followup_behavior_test'
    'peri-agent:test:compact_failure_adversarial_test'
    'peri-agent:lib:*'
    'peri-acp:lib:*'
    'peri-tui:test:meta_session_cli'
    'peri-middlewares:lib:*'
    # 以下目标是本地 mock / 临时 SQLite / 真实进程回归，不依赖外部模型 SDK。
    # 单列后，CI 过滤器一旦漏掉某个目标会先在这里失败。
    'peri-acp:test:compact_command_contract_test'
    'peri-acp:test:concurrent_bg_agent_test'
    'peri-acp:test:integration_test'
    'peri-acp:test:prompt_cache_boundary'
    'peri-agent:test:micro_compact_wire_test'
    'peri-agent:test:projection_allocation_test'
    'peri-agent:test:responses_terminal_test'
    'peri-agent:test:stream_interruption_logs_test'
    'peri-agent:test:stream_interruption_test'
    'peri-middlewares:test:canonical_tool_invocation_contract'
    'peri-middlewares:test:hook_output_diagnostic_contract'
    'peri-middlewares:test:mcp_host_policy_contract'
    'peri-middlewares:test:mcp_isolation_contract'
    'peri-middlewares:test:subagent_mcp_visibility'
    'peri-middlewares:test:workspace_tool_registration'
    'peri-tui:test:cli_session_args'
    'peri-tui:test:print_background_exit'
    'peri-tui:test:print_bare'
    'peri-tui:test:print_exit'
    'peri-tui:test:session_recovery_process'
)

# 逐 binary 非零门禁的目标：ci.yml 必须为每项保留一条只选中该目标的 `-E`，
# 并逐次调用 nextest。合并成 union 时 `--no-tests=fail` 只约束合并结果，其中
# 任何一套为空都不会失败。
per_binary_suites=(
    'peri-agent:test:compact_pressure_adversarial_test'
    'peri-agent:test:compact_session_adversarial_test'
    'peri-agent:test:compact_session_manual_adversarial_test'
    'peri-agent:test:compact_authority_projection_test'
)

# metadata 经临时文件传给 Python，不作为命令行参数：本仓库的 workspace 已让
# `cargo metadata --no-deps` 输出约 148KB，超过 Linux 的单字符串上限
# （MAX_ARG_STRLEN = 128KB），直接传参会 E2BIG（Argument list too long）；
# 环境变量受同一上限约束，故两者都不用。
metadata_file="$(mktemp)"
trap 'rm -f "$metadata_file"' EXIT
cargo metadata --no-deps --format-version 1 > "$metadata_file"

WORKFLOW="$workflow" \
EXCLUDED_SET="$(printf '%s\n' "${workspace_excluded[@]}")" \
INCLUDES="$(printf '%s\n' "${step_includes[@]}")" \
DOCTEST_EXCLUDED="$(printf '%s\n' "${doctest_excluded[@]}")" \
REQUIRED="$(printf '%s\n' "${required_suites[@]}")" \
PER_BINARY="$(printf '%s\n' "${per_binary_suites[@]}")" \
AS_JSON="$as_json" \
python3 "$repo_root/scripts/ci_test_inventory.py" "$metadata_file"
