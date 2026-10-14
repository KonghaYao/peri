#!/usr/bin/env bash
# S13 任务 B4：拆分后「挂载真实成立」证明——逐条过滤运行，要求非 0 filtered / 非空跑。
set -uo pipefail
cd "$(dirname "$0")/../.."
OUT=.recon/s13-size-tui-resources-raw/mount-proof.txt
: > "$OUT"

run() {
    echo "\$ ./scripts/cargo-rmcp-patched.sh test --locked $*" >> "$OUT"
    ./scripts/cargo-rmcp-patched.sh test --locked "$@" >> "$OUT" 2>&1
    echo "EXIT=$?" >> "$OUT"
    echo >> "$OUT"
}

# peri-tui：session_events_test 三块 + 生产 system 拆分的两条既有调用链
run -p peri-tui --lib -- acp_events_test::session_events_test::
run -p peri-tui --lib -- acp_events_test::session_events_test::session_rewind_events_test::
run -p peri-tui --lib -- acp_events_test::session_events_test::session_compact_events_test::
run -p peri-tui --lib -- acp_events_test::bg_task_live_test::
run -p peri-tui --lib -- acp_events_test::interaction_test::test_hitl_input_summary_extraction_matrix

# peri-resources：full_remote 父子模块 + perf 父子模块
run -p peri-resources --lib -- sessions::remote::schema_upgrade_tests::full_remote_tests::
run -p peri-resources --lib -- sessions::remote::schema_upgrade_tests::full_remote_tests::full_remote_rewind_tests::
run -p peri-resources --lib -- history_perf_tests::
run -p peri-resources --lib -- history_perf_tests::cases::history_segment_statements_are_index_located
run -p peri-resources --lib -- history_perf_tests::audit::audit_segment_lookahead_and_plan_shape --ignored --nocapture
