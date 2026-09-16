//! `Bash` 三字段解析与 timeout 语义（FC-BASH-01）。
//!
//! 事实源：`peri-middlewares/src/middleware/terminal.rs` 的 `parameters()` 与
//! `peri-agent/src/agent/async_tasks/shell.rs::parse_timeout`。
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
fn timeout_parsing_clamps_and_zero_disables() {
    // 前台：0 = 不超时；>0 clamp 到 [1, 600000]。
    assert_eq!(parse_timeout(&json!({ "timeout": 0 }), false), None);
    assert_eq!(parse_timeout(&json!({ "timeout": 1 }), false), Some(1));
    assert_eq!(
        parse_timeout(&json!({ "timeout": 600_000 }), false),
        Some(600_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 900_000 }), false),
        Some(600_000),
        "超过上限应被 clamp"
    );
    assert_eq!(parse_timeout(&json!({}), false), Some(15_000));

    // 后台：未传或 0 = 不超时；显式 >0 clamp。
    assert_eq!(parse_timeout(&json!({}), true), None);
    assert_eq!(parse_timeout(&json!({ "timeout": 0 }), true), None);
    assert_eq!(
        parse_timeout(&json!({ "timeout": 2_000 }), true),
        Some(2_000)
    );
    assert_eq!(
        parse_timeout(&json!({ "timeout": 999_999 }), true),
        Some(600_000)
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
