use serde_json::json;
use serial_test::serial;

use super::*;

fn restore(previous: Option<ContextUsage>) {
    *CONTEXT_USAGE.state().write() = previous;
}

/// 有保存值：进入会话即用已保存 token 与当前 window 计算百分比。
#[test]
#[serial]
fn saved_value_is_shown_with_the_current_window() {
    let previous = *CONTEXT_USAGE.state().read();
    let usage = from_load_response(&json!({
        "_meta": {"peri.contextUsage": {"tokens": 50_000, "window": 200_000}},
    }));
    establish_session_usage(usage);
    let shown = (*CONTEXT_USAGE.state().read()).expect("进入会话后必须有展示值");
    assert_eq!(
        shown,
        ContextUsage::Known {
            tokens: 50_000,
            window: 200_000,
        }
    );
    assert_eq!(shown.percent(), Some(25.0));
    restore(previous);
}

/// 值为 NULL：显式未知，不能落到 0%。
#[test]
#[serial]
fn null_saved_value_is_unknown_not_zero() {
    let previous = *CONTEXT_USAGE.state().read();
    let usage = from_load_response(&json!({
        "_meta": {"peri.contextUsage": {"tokens": null, "window": 200_000}},
    }));
    establish_session_usage(usage);
    let shown = (*CONTEXT_USAGE.state().read()).expect("未知也是明确的展示状态");
    assert_eq!(shown, ContextUsage::Unknown);
    assert_eq!(shown.percent(), None, "未知不得被当成 0%");
    restore(previous);
}

/// window 变化：同一批 token 的百分比按当前有效 window 重算。
#[test]
#[serial]
fn percentage_follows_the_current_window() {
    let previous = *CONTEXT_USAGE.state().read();
    establish_session_usage(ContextUsage::Known {
        tokens: 50_000,
        window: 200_000,
    });
    assert_eq!(
        (*CONTEXT_USAGE.state().read()).expect("已保存值").percent(),
        Some(25.0)
    );

    apply_state_snapshot(50_000, true, Some(400_000));

    assert_eq!(
        (*CONTEXT_USAGE.state().read())
            .expect("运行期快照")
            .percent(),
        Some(12.5),
        "百分比必须按新的有效 window 计算"
    );
    restore(previous);
}
