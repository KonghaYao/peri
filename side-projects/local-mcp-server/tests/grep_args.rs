//! Grep 参数层：字段、默认值、别名优先级与数值校验（FC-GREP-01/02/03）。
//!
//! 事实源：`peri-middlewares/src/tools/filesystem/{grep.rs,grep_args.rs}`。
//! 每个断言都对应源实现中的一行可复核语义，不做"更合理"的修正。

use local_mcp_server::tools::grep::args::{
    parse_optional_u64, type_to_glob, GrepInput, OutputMode, DEFAULT_HEAD_LIMIT,
};
use serde_json::json;

fn parse(value: serde_json::Value) -> GrepInput {
    GrepInput::from_arguments(&value).expect("参数应解析成功")
}

#[test]
fn minimal_call_uses_source_defaults() {
    let input = parse(json!({ "pattern": "needle" }));
    assert_eq!(input.pattern, "needle");
    assert_eq!(input.path, None);
    assert_eq!(input.glob, None);
    assert_eq!(input.type_filter, None);
    assert_eq!(input.output_mode, None);
    assert!(!input.case_insensitive);
    assert_eq!(input.context, None);
    assert_eq!(input.before_context, None);
    assert_eq!(input.after_context, None);
    // 源：`show_line_numbers` 默认 true。
    assert!(input.line_number);
    assert!(!input.multiline);
    assert!(!input.whole_word);
    assert!(!input.invert_match);
    assert!(!input.fixed_strings);
    // 源：`head_limit` 默认 250；`offset`/`max_depth` 未传 = None。
    assert_eq!(input.head_limit, DEFAULT_HEAD_LIMIT);
    assert_eq!(input.head_limit, 250);
    assert_eq!(input.offset, None);
    assert_eq!(input.max_depth, None);
}

#[test]
fn missing_pattern_uses_source_message() {
    let error = GrepInput::from_arguments(&json!({ "path": "." })).expect_err("应拒绝");
    assert_eq!(error.message, "Error: Missing required parameter 'pattern'");
    let error = GrepInput::from_arguments(&json!({ "pattern": 7 })).expect_err("应拒绝");
    assert_eq!(error.message, "Error: Missing required parameter 'pattern'");
}

#[test]
fn semantic_aliases_take_priority_over_cli_style() {
    // 语义键存在时独占解析（源 `input.get("case_insensitive").or_else(|| input.get("-i"))`）。
    let input = parse(json!({ "pattern": "p", "case_insensitive": true, "-i": false }));
    assert!(input.case_insensitive);
    let input = parse(json!({ "pattern": "p", "case_insensitive": false, "-i": true }));
    assert!(!input.case_insensitive);

    let input = parse(json!({ "pattern": "p", "show_line_numbers": false, "-n": true }));
    assert!(!input.line_number);
    let input = parse(json!({ "pattern": "p", "-n": false }));
    assert!(!input.line_number);
    assert!(parse(json!({ "pattern": "p" })).line_number);

    // 数值别名：语义键优先（`context` vs `-C`）。
    let input = parse(json!({ "pattern": "p", "context": 2, "-C": 5 }));
    assert_eq!(input.context, Some(2));
    let input = parse(json!({ "pattern": "p", "-C": 5 }));
    assert_eq!(input.context, Some(5));

    let input = parse(json!({ "pattern": "p", "before_context": 1, "-B": 9 }));
    assert_eq!(input.before_context, Some(1));
    let input = parse(json!({ "pattern": "p", "-B": 9 }));
    assert_eq!(input.before_context, Some(9));

    let input = parse(json!({ "pattern": "p", "after_context": 3, "-A": 8 }));
    assert_eq!(input.after_context, Some(3));
    let input = parse(json!({ "pattern": "p", "-A": 8 }));
    assert_eq!(input.after_context, Some(8));
}

#[test]
fn semantic_alias_with_wrong_type_does_not_fall_back_to_cli() {
    // 源：`input.get(语义键).or_else(CLI).and_then(as_bool).unwrap_or(default)`。
    // 语义键存在但类型不合法 → 取默认值，**不**回落到 CLI 键。
    let input = parse(json!({ "pattern": "p", "case_insensitive": "yes", "-i": true }));
    assert!(!input.case_insensitive);
    let input = parse(json!({ "pattern": "p", "show_line_numbers": "no", "-n": false }));
    assert!(input.line_number);
}

#[test]
fn numeric_params_reject_fractional_and_negative() {
    let error = GrepInput::from_arguments(&json!({ "pattern": "p", "context": 1.5 }))
        .expect_err("小数应被拒绝");
    assert_eq!(
        error.message,
        "Error: 'context' must be a non-negative integer, got 1.5"
    );

    let error = GrepInput::from_arguments(&json!({ "pattern": "p", "head_limit": -3 }))
        .expect_err("负数应被拒绝");
    assert_eq!(
        error.message,
        "Error: 'head_limit' must be a non-negative integer, got -3"
    );

    let error = GrepInput::from_arguments(&json!({ "pattern": "p", "max_depth": "deep" }))
        .expect_err("非数值应被拒绝");
    assert_eq!(
        error.message,
        r#"Error: 'max_depth' must be a non-negative integer, got "deep""#
    );

    // 整数允许 0（源：`0` 合法；`head_limit = 0` 表示 unlimited）。
    let input = parse(json!({ "pattern": "p", "head_limit": 0, "offset": 0, "max_depth": 0 }));
    assert_eq!(input.head_limit, 0);
    assert_eq!(input.offset, Some(0));
    assert_eq!(input.max_depth, Some(0));
}

#[test]
fn null_numeric_params_are_treated_as_absent() {
    assert_eq!(parse_optional_u64(&json!(null), "offset").unwrap(), None);
    let input = parse(json!({ "pattern": "p", "offset": null, "max_depth": null }));
    assert_eq!(input.offset, None);
    assert_eq!(input.max_depth, None);
}

#[test]
fn output_mode_enum_and_error_text() {
    assert_eq!(OutputMode::parse("content").unwrap(), OutputMode::Default);
    assert_eq!(
        OutputMode::parse("files_with_matches").unwrap(),
        OutputMode::FilesOnly
    );
    assert_eq!(OutputMode::parse("count").unwrap(), OutputMode::CountOnly);
    assert_eq!(
        OutputMode::parse("files_without_matches").unwrap(),
        OutputMode::FilesWithoutMatch
    );

    let error = OutputMode::parse("files").expect_err("非法 mode");
    assert_eq!(
        error.message,
        "Invalid output_mode: 'files'. Must be 'content', 'files_with_matches', 'count', or 'files_without_matches'"
    );

    // 经 `to_parsed_args` 的错误路径同样逐字。
    let input = parse(json!({ "pattern": "p", "output_mode": "files" }));
    assert!(input.to_parsed_args().is_err());

    // 默认 content。
    let parsed = parse(json!({ "pattern": "p" })).to_parsed_args().unwrap();
    assert_eq!(parsed.output_mode, OutputMode::Default);
}

#[test]
fn type_filter_maps_to_source_glob_table() {
    assert_eq!(type_to_glob("rust"), vec!["*.rs"]);
    assert_eq!(type_to_glob("js"), vec!["*.js", "*.mjs"]);
    assert_eq!(type_to_glob("ts"), vec!["*.ts", "*.tsx"]);
    assert_eq!(type_to_glob("cpp"), vec!["*.cpp", "*.hpp", "*.cc", "*.cxx"]);
    assert_eq!(type_to_glob("markdown"), vec!["*.md", "*.mdx"]);
    assert_eq!(type_to_glob("md"), vec!["*.md", "*.mdx"]);
    // 未知 type：空映射 = 不加过滤器（源事实，保留）。
    assert!(type_to_glob("brainfuck").is_empty());

    let parsed = parse(json!({ "pattern": "p", "glob": "*.rs", "type": "ts" }))
        .to_parsed_args()
        .unwrap();
    assert_eq!(parsed.glob_filters, vec!["*.rs", "*.ts", "*.tsx"]);

    let parsed = parse(json!({ "pattern": "p", "type": "unknown-type" }))
        .to_parsed_args()
        .unwrap();
    assert!(parsed.glob_filters.is_empty());
}

#[test]
fn context_alias_expands_symmetrically_unless_asymmetric_present() {
    // 只有 `-C`/`context`：before = after = C。
    let parsed = parse(json!({ "pattern": "p", "context": 4 }))
        .to_parsed_args()
        .unwrap();
    assert_eq!(parsed.before_context, 4);
    assert_eq!(parsed.after_context, 4);

    // 出现任一非对称键：对称键被忽略，缺的一侧为 0。
    let parsed = parse(json!({ "pattern": "p", "context": 4, "after_context": 2 }))
        .to_parsed_args()
        .unwrap();
    assert_eq!(parsed.before_context, 0);
    assert_eq!(parsed.after_context, 2);

    let parsed = parse(json!({ "pattern": "p", "context": 4, "before_context": 1 }))
        .to_parsed_args()
        .unwrap();
    assert_eq!(parsed.before_context, 1);
    assert_eq!(parsed.after_context, 0);
}

#[test]
fn boolean_flags_carry_through_to_parsed_args() {
    let parsed = parse(json!({
        "pattern": "p",
        "multiline": true,
        "whole_word": true,
        "invert_match": true,
        "fixed_strings": true,
        "max_depth": 3,
    }))
    .to_parsed_args()
    .unwrap();
    assert!(parsed.multiline);
    assert!(parsed.whole_word);
    assert!(parsed.invert_match);
    assert!(parsed.fixed_strings);
    assert_eq!(parsed.max_depth, Some(3));
    assert!(!parsed.case_insensitive);
    assert!(parsed.line_number);
}

#[test]
fn unknown_fields_are_ignored() {
    let input = parse(json!({
        "pattern": "p",
        "-g": "*.rs",
        "-t": "rust",
        "type_excludes": ["x"],
        "no_ignore": true,
        "unknown": { "nested": 1 },
    }));
    assert_eq!(input.glob, None);
    assert_eq!(input.type_filter, None);
    assert_eq!(input.pattern, "p");
}

#[test]
fn schema_field_names_match_frozen_fixture() {
    // 夹具 `tests/fixtures/schemas/grep.json` 的 properties 必须与实现可解析的
    // 字段集合一致（防止实现悄悄改名）。
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/schemas/grep.json")).expect("夹具 JSON");
    let properties = fixture["inputSchema"]["properties"]
        .as_object()
        .expect("properties 对象");
    let mut declared: Vec<&str> = properties.keys().map(|key| key.as_str()).collect();
    declared.sort_unstable();

    let mut expected = vec![
        "pattern",
        "path",
        "glob",
        "type",
        "output_mode",
        "-i",
        "case_insensitive",
        "-C",
        "context",
        "-A",
        "after_context",
        "-B",
        "before_context",
        "-n",
        "show_line_numbers",
        "multiline",
        "whole_word",
        "invert_match",
        "fixed_strings",
        "max_depth",
        "head_limit",
        "offset",
    ];
    expected.sort_unstable();
    assert_eq!(declared, expected);

    // `required` 恰好是 pattern。
    assert_eq!(fixture["inputSchema"]["required"], json!(["pattern"]));
    // output_mode 的 enum 顺序即源枚举顺序。
    assert_eq!(
        fixture["inputSchema"]["properties"]["output_mode"]["enum"],
        json!([
            "content",
            "files_with_matches",
            "count",
            "files_without_matches"
        ])
    );
    assert_eq!(
        fixture["inputSchema"]["properties"]["head_limit"]["minimum"],
        json!(0)
    );
    assert_eq!(
        fixture["inputSchema"]["properties"]["max_depth"]["minimum"],
        json!(0)
    );
}
