//! `runtime/params.rs` 的行为测试：`parse_start_input` 的解析与校验边界
//! （自 `tool_test.rs` 迁入的解析类用例 + scriptPath / resume 安全边界）。

use super::*;

const SCRIPT: &str = "export const meta = { name: 'demo', description: 'demo' }; return 1";

fn temp_cwd() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

#[test]
fn strict_preflight_is_rejected_before_any_io() {
    let tmp = temp_cwd();
    let error = parse_start_input(
        &serde_json::json!({ "script": SCRIPT, "strictPreflight": true }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("cannot be statically validated"), "{error}");
}

#[test]
fn missing_script_surfaces_required_field_error() {
    let tmp = temp_cwd();
    let error =
        parse_start_input(&serde_json::json!({}), tmp.path().to_str().unwrap()).unwrap_err();
    assert_eq!(error, "missing 'script' or 'scriptPath' field");
}

#[test]
fn max_concurrency_bounds_reject_over_cap_and_zero() {
    let tmp = temp_cwd();
    let cwd = tmp.path().to_str().unwrap();
    for invalid in [
        serde_json::json!(0),
        serde_json::json!(MAX_CONCURRENCY_CAP + 1),
        serde_json::json!(1.5),
        serde_json::json!(2.5),
        serde_json::json!("3"),
    ] {
        let error = parse_start_input(
            &serde_json::json!({ "script": SCRIPT, "maxConcurrency": invalid }),
            cwd,
        )
        .unwrap_err();
        assert!(error.contains("maxConcurrency"), "{error}");
    }
    let parsed = parse_start_input(
        &serde_json::json!({ "script": SCRIPT, "maxConcurrency": MAX_CONCURRENCY_CAP }),
        cwd,
    )
    .unwrap();
    assert_eq!(parsed.max_concurrency, MAX_CONCURRENCY_CAP as u32);
    // 缺省为 3（schema default 与解析一致）。
    let defaulted = parse_start_input(&serde_json::json!({ "script": SCRIPT }), cwd).unwrap();
    assert_eq!(defaulted.max_concurrency, 3);
}

#[test]
fn max_concurrency_schema_matches_runtime_bounds() {
    let schema = start_tool_parameters();
    let field = &schema["properties"]["maxConcurrency"];
    assert_eq!(field["type"], "integer");
    assert_eq!(field["minimum"].as_u64(), Some(1));
    assert_eq!(field["maximum"].as_u64(), Some(MAX_CONCURRENCY_CAP));
    assert_eq!(field["default"].as_u64(), Some(3));
}

#[test]
fn resume_only_accepts_run_id_and_false_strict_preflight() {
    let run_id = uuid::Uuid::now_v7().to_string();
    validate_resume_only_input(&serde_json::json!({
        "resumeFromRunId": run_id,
        "strictPreflight": false
    }))
    .unwrap();
    // 显式 null 视为未提供，不是覆盖请求。
    validate_resume_only_input(&serde_json::json!({
        "resumeFromRunId": uuid::Uuid::now_v7().to_string(),
        "maxAgents": null,
        "args": null
    }))
    .unwrap();
}

#[test]
fn resume_only_rejects_every_start_only_field() {
    let run_id = uuid::Uuid::now_v7().to_string();
    let rejected = [
        ("args", serde_json::json!({"changed": true})),
        ("maxConcurrency", serde_json::json!(2)),
        ("budgetTotal", serde_json::json!(1000)),
        ("maxAgents", serde_json::json!(1)),
        ("maxToolCalls", serde_json::json!(1)),
        ("maxElapsedMs", serde_json::json!(1000)),
        ("name", serde_json::json!("changed")),
        ("writeIntent", serde_json::json!({"kind": "read_only"})),
        ("script", serde_json::json!("return 1")),
        ("scriptPath", serde_json::json!("changed.js")),
        ("script", serde_json::json!(42)),
    ];
    for (field, value) in rejected {
        let input = serde_json::json!({ "resumeFromRunId": run_id, (field): value });
        let error = validate_resume_only_input(&input).unwrap_err();
        assert!(error.contains(field), "{field}: {error}");
        assert!(error.contains("resume-only"), "{field}: {error}");
        assert!(
            error.contains("script + resumeFromRunId"),
            "{field}: {error}"
        );
    }
}

#[test]
fn resume_only_rejects_strict_preflight_true_with_start_message() {
    let tmp = temp_cwd();
    let start_error = parse_start_input(
        &serde_json::json!({ "script": SCRIPT, "strictPreflight": true }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap_err();
    let resume_error = validate_resume_only_input(&serde_json::json!({
        "resumeFromRunId": uuid::Uuid::now_v7().to_string(),
        "strictPreflight": true
    }))
    .unwrap_err();
    assert_eq!(resume_error, start_error);
    assert!(resume_error.contains("strict preflight"), "{resume_error}");
}

#[test]
fn resume_from_run_id_requires_uuid_without_traversal() {
    let tmp = temp_cwd();
    let cwd = tmp.path().to_str().unwrap();
    for invalid in ["../../etc/passwd", "not-a-uuid", "abc/def"] {
        let error = parse_start_input(
            &serde_json::json!({ "script": SCRIPT, "resumeFromRunId": invalid }),
            cwd,
        )
        .unwrap_err();
        assert!(error.contains("Invalid resumeFromRunId"), "{error}");
    }
    let run_id = uuid::Uuid::now_v7().to_string();
    let parsed = parse_start_input(
        &serde_json::json!({ "script": SCRIPT, "resumeFromRunId": run_id }),
        cwd,
    )
    .unwrap();
    assert_eq!(parsed.resume_from.as_deref(), Some(run_id.as_str()));
}

#[test]
fn explicit_name_wins_over_script_heuristic() {
    let tmp = temp_cwd();
    let cwd = tmp.path().to_str().unwrap();
    let parsed = parse_start_input(
        &serde_json::json!({ "script": SCRIPT, "name": "explicit" }),
        cwd,
    )
    .unwrap();
    assert_eq!(parsed.workflow_name, "explicit");

    let heuristic = parse_start_input(&serde_json::json!({ "script": SCRIPT }), cwd).unwrap();
    assert_eq!(heuristic.workflow_name, "demo");

    let unnamed = parse_start_input(&serde_json::json!({ "script": "return 1" }), cwd).unwrap();
    assert_eq!(unnamed.workflow_name, "unnamed");
}

#[test]
fn script_path_must_stay_inside_cwd() {
    let tmp = temp_cwd();
    let outside = temp_cwd();
    let outside_script = outside.path().join("script.js");
    std::fs::write(&outside_script, SCRIPT).unwrap();

    let error = parse_start_input(
        &serde_json::json!({ "scriptPath": outside_script.to_str().unwrap() }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("outside the working directory"), "{error}");

    let error = parse_start_input(
        &serde_json::json!({ "scriptPath": "../escape.js" }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("path not found"), "{error}");
}

#[test]
fn script_path_inside_cwd_wins_over_inline_script() {
    let tmp = temp_cwd();
    let script_file = tmp.path().join("named.js");
    std::fs::write(&script_file, SCRIPT).unwrap();
    let parsed = parse_start_input(
        &serde_json::json!({
            "script": "export const meta = { name: 'inline' }; return 2",
            "scriptPath": "named.js"
        }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(parsed.script, SCRIPT, "scriptPath 优先于 inline script");
    assert_eq!(parsed.workflow_name, "demo");
}

#[test]
fn invalid_write_intent_is_rejected_before_start() {
    let tmp = temp_cwd();
    let error = parse_start_input(
        &serde_json::json!({
            "script": SCRIPT,
            "writeIntent": { "kind": "write" }
        }),
        tmp.path().to_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("Invalid writeIntent"), "{error}");
}

#[test]
fn budget_total_accepts_omitted_and_safe_integer_bounds() {
    assert_eq!(parse_budget_total(&serde_json::json!({})).unwrap(), None);
    assert_eq!(
        parse_budget_total(&serde_json::json!({"budgetTotal": 1})).unwrap(),
        Some(1)
    );
    assert_eq!(
        parse_budget_total(&serde_json::json!({
            "budgetTotal": MAX_SAFE_BUDGET_TOTAL
        }))
        .unwrap(),
        Some(MAX_SAFE_BUDGET_TOTAL)
    );
}

#[test]
fn budget_total_rejects_invalid_values_with_stable_range_error() {
    let invalid = [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1000"),
        serde_json::json!(true),
        serde_json::json!(MAX_SAFE_BUDGET_TOTAL + 1),
    ];

    for value in invalid {
        let error = parse_budget_total(&serde_json::json!({"budgetTotal": value})).unwrap_err();
        assert_eq!(
            error,
            format!("'budgetTotal' must be an integer between 1 and {MAX_SAFE_BUDGET_TOTAL}")
        );
    }
}

#[test]
fn host_limits_reject_invalid_values() {
    for field in ["maxAgents", "maxToolCalls", "maxElapsedMs"] {
        let input = serde_json::json!({(field): 0});
        let error = parse_bounded_integer(&input, field, None, MAX_SAFE_INTEGER).unwrap_err();
        assert!(error.contains(field));
    }
    assert!(parse_bounded_integer(
        &serde_json::json!({"maxConcurrency": 17}),
        "maxConcurrency",
        Some(3),
        MAX_CONCURRENCY_CAP,
    )
    .is_err());
}
