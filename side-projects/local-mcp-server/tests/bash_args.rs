//! `Bash` 三字段解析与 timeout 语义（FC-BASH-01）。
//!
//! 事实源：`peri-middlewares/src/middleware/terminal.rs` 的 `parameters()` 与
//! `peri-agent/src/agent/async_tasks/shell.rs` 的 `parse_foreground_timeout` /
//! `parse_background_timeout`（同步路径恒有界；`timeout: 0` 只对后台表示"不超时"）。
//! 公开输入严格三字段：`command`、`timeout`、`run_in_background`——本测试同时
//! 断言"多余字段被忽略"，防止任务控制被偷偷塞进 Bash 输入。

use local_mcp_server::tools::bash::{parse_arguments, parse_timeout};
use serde_json::json;

#[test]
fn three_fields_only_and_required_command() {
    let args = parse_arguments(&json!({ "command": "printf hi" })).expect("解析成功");
    assert_eq!(args.command, "printf hi");
    assert_eq!(args.timeout_ms, Some(15_000), "前台默认 15s");
    assert!(!args.background);

    let error = parse_arguments(&json!({ "timeout": 100 })).expect_err("缺 command");
    assert_eq!(error, "Missing command parameter");
    let error = parse_arguments(&json!({ "command": 3 })).expect_err("类型错误");
    assert_eq!(error, "Missing command parameter");
    let error = parse_arguments(&json!({})).expect_err("空对象");
    assert_eq!(error, "Missing command parameter");
}

#[test]
fn schema_declares_exactly_three_fields() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/schemas/bash.json")).expect("夹具 JSON");
    let properties = fixture["inputSchema"]["properties"]
        .as_object()
        .expect("properties");
    let mut names: Vec<&str> = properties.keys().map(|key| key.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["command", "run_in_background", "timeout"]);
    assert_eq!(fixture["inputSchema"]["required"], json!(["command"]));

    // 别名只登记 `Shell`（不进 tools/list，由 wire.rs 冻结）。
    assert_eq!(fixture["aliases"], json!(["Shell"]));
}

#[test]
fn background_flag_and_timeout_defaults() {
    let args = parse_arguments(&json!({ "command": "sleep 1", "run_in_background": true }))
        .expect("解析成功");
    assert!(args.background);
    assert_eq!(args.timeout_ms, None, "显式后台默认不超时");

    let args = parse_arguments(&json!({ "command": "sleep 1", "run_in_background": false }))
        .expect("解析成功");
    assert!(!args.background);
    assert_eq!(args.timeout_ms, Some(15_000));

    // 非布尔 run_in_background 视为未传（源 `as_bool().unwrap_or(false)`）。
    let args =
        parse_arguments(&json!({ "command": "x", "run_in_background": "yes" })).expect("解析成功");
    assert!(!args.background);
}

#[test]
fn foreground_timeout_is_always_bounded() {
    use local_mcp_server::tools::bash::{foreground_timeout_note, MAX_FOREGROUND_TIMEOUT_MS};
    assert_eq!(MAX_FOREGROUND_TIMEOUT_MS, 120_000);

    // 前台（同步）恒有界：默认 15000，`0` 与超上限都界到 120000——不存在禁用超时的路径。
    assert_eq!(
        parse_timeout(&json!({ "timeout": 0 }), false),
        Some(120_000)
    );
    assert_eq!(parse_timeout(&json!({ "timeout": 1 }), false), Some(1));
    assert_eq!(
        parse_timeout(&json!({ "timeout": 120_000 }), false),
        Some(120_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 600_000 }), false),
        Some(120_000),
        "超过前台上限应界到上限"
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 900_000 }), false),
        Some(120_000)
    );
    assert_eq!(parse_timeout(&json!({}), false), Some(15_000));
    // 前台返回值恒为 `Some`：任何输入都不得产生"不超时"的同步等待。
    for value in [
        json!({}),
        json!({ "timeout": 0 }),
        json!({ "timeout": 1 }),
        json!({ "timeout": 900_000 }),
        json!({ "timeout": "1000" }),
        json!({ "timeout": -5 }),
        json!({ "timeout": null }),
    ] {
        assert!(
            parse_timeout(&value, false).is_some(),
            "前台解析结果必须恒有界: {value}"
        );
    }

    // 回执说明（源 `foreground_timeout_note`）：仅在请求被改写时非空。
    assert_eq!(foreground_timeout_note(&json!({})), "");
    assert_eq!(foreground_timeout_note(&json!({ "timeout": 2_000 })), "");
    assert_eq!(foreground_timeout_note(&json!({ "timeout": 120_000 })), "");
    let zero = foreground_timeout_note(&json!({ "timeout": 0 }));
    assert!(
        zero.contains("`timeout: 0` cannot disable the timeout")
            && zero.contains(&MAX_FOREGROUND_TIMEOUT_MS.to_string()),
        "0 被界到上限时必须说明: {zero}"
    );
    let oversized = foreground_timeout_note(&json!({ "timeout": 600_000 }));
    assert!(
        oversized.contains("exceeds the foreground maximum")
            && oversized.contains("600000")
            && oversized.contains(&MAX_FOREGROUND_TIMEOUT_MS.to_string()),
        "超上限被界住时必须说明请求值与上限: {oversized}"
    );

    // 非数值/负数/浮点：`as_u64` 取不到值 → 按未传处理（源语义）。
    assert_eq!(
        parse_timeout(&json!({ "timeout": "1000" }), false),
        Some(15_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": -5 }), false),
        Some(15_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 1.5 }), false),
        Some(15_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": null }), false),
        Some(15_000)
    );
}

#[test]
fn background_timeout_defaults_to_unbounded() {
    // 后台：未传或 0 = 不超时；显式 >0 clamp 到 [1, 600000]。
    assert_eq!(parse_timeout(&json!({}), true), None);
    assert_eq!(parse_timeout(&json!({ "timeout": 0 }), true), None);
    assert_eq!(
        parse_timeout(&json!({ "timeout": 2_000 }), true),
        Some(2_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 600_000 }), true),
        Some(600_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 999_999 }), true),
        Some(600_000),
        "后台显式上限仍是 600000"
    );
}

#[test]
fn legacy_and_unknown_fields_are_ignored() {
    // 源测试 `test_bash_legacy_params_ignored` / `test_bash_schema_no_legacy_params`。
    let args = parse_arguments(&json!({
        "command": "printf hi",
        "run_in_background": false,
        "timeout": 5_000,
        "wait_for_completion": true,
        "description": "legacy",
        "cwd": "/etc",
        "background": true,
        "task_id": "shell-123",
    }))
    .expect("解析成功");
    assert_eq!(args.command, "printf hi");
    assert_eq!(args.timeout_ms, Some(5_000));
    assert!(!args.background, "忽略 legacy/未知字段");
}
