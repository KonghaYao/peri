//! ArgsSchema / ArgKind / FlagSpec serde 模型测试：完整形状往返（object
//! 数组形态）、缺省字段默认值、四类 kind、externally tagged wire 形态锁定、
//! skip_serializing_if 生效、snake_case 键名稳定。

use super::*;

/// 完整形状 fixture：positionals×1 + named×2 + flags×1 混合。
fn full_schema() -> ArgsSchema {
    ArgsSchema {
        positionals: vec![ArgSpec {
            name: "query".into(),
            kind: ArgKind::String,
            required: true,
            description: Some("搜索关键词".into()),
        }],
        named: vec![
            ArgSpec {
                name: "model".into(),
                kind: ArgKind::Choice(vec!["claude".into(), "gpt".into()]),
                required: false,
                description: None,
            },
            ArgSpec {
                name: "depth".into(),
                kind: ArgKind::Int,
                required: true,
                description: Some("递归深度".into()),
            },
        ],
        flags: vec![FlagSpec {
            name: "force".into(),
            short: Some("-f".into()),
            description: Some("强制覆盖".into()),
        }],
    }
}

// ─── serde 往返：完整形状（object 数组形态）───────────────────────────────

#[test]
fn full_shape_serde_roundtrip() {
    let schema = full_schema();
    let json = serde_json::to_string(&schema).unwrap();
    let back: ArgsSchema = serde_json::from_str(&json).unwrap();
    assert_eq!(back, schema, "完整形状 JSON 往返后应逐字段相等");
}

// ─── 缺省字段默认值 ───────────────────────────────────────────────────────

#[test]
fn args_schema_empty_object_deserializes_to_default() {
    let schema: ArgsSchema = serde_json::from_str("{}").unwrap();
    assert_eq!(schema, ArgsSchema::default());
    assert!(schema.positionals.is_empty());
    assert!(schema.named.is_empty());
    assert!(schema.flags.is_empty());
}

#[test]
fn args_schema_missing_dimensions_default_to_empty() {
    let schema: ArgsSchema =
        serde_json::from_str(r#"{"named":[{"name":"x","kind":"Int"}]}"#).unwrap();
    assert!(schema.positionals.is_empty());
    assert_eq!(schema.named.len(), 1);
    assert!(schema.flags.is_empty());
}

#[test]
fn arg_spec_missing_optional_fields_default_to_false_and_none() {
    let spec: ArgSpec = serde_json::from_str(r#"{"name":"query","kind":"String"}"#).unwrap();
    assert!(!spec.required);
    assert_eq!(spec.description, None);
}

#[test]
fn flag_spec_missing_optional_fields_default_to_none() {
    let flag: FlagSpec = serde_json::from_str(r#"{"name":"force"}"#).unwrap();
    assert_eq!(flag.short, None);
    assert_eq!(flag.description, None);
}

#[test]
fn flag_spec_explicit_null_optional_fields_deserialize_to_none() {
    // 显式 null 与缺省等价（Option 语义），外部声明方不必区分。
    let flag: FlagSpec =
        serde_json::from_str(r#"{"name":"force","short":null,"description":null}"#).unwrap();
    assert_eq!(flag.short, None);
    assert_eq!(flag.description, None);
}

// ─── 四类 kind 往返 + wire 形态锁定 ───────────────────────────────────────

#[test]
fn all_four_arg_kinds_roundtrip() {
    let kinds = [
        ArgKind::String,
        ArgKind::Int,
        ArgKind::Choice(vec![]),
        ArgKind::Choice(vec!["a".into(), "b".into()]),
        ArgKind::Path,
    ];
    for kind in kinds {
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArgKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind, "kind {json} 往返应相等");
    }
}

#[test]
fn choice_wire_form_is_externally_tagged_object() {
    let kind = ArgKind::Choice(vec!["a".into(), "b".into()]);
    let json = serde_json::to_string(&kind).unwrap();
    assert_eq!(json, r#"{"Choice":["a","b"]}"#);
}

#[test]
fn choice_empty_list_wire_form_is_object_with_empty_array() {
    // 空候选列表 = 运行时由 handler 补充候选（command_args.rs 注释定案），
    // wire 形态与多元素一致，锁定防 Phase 5 解析器另立语义。
    let json = serde_json::to_string(&ArgKind::Choice(Vec::<String>::new())).unwrap();
    assert_eq!(json, r#"{"Choice":[]}"#);
    let back: ArgKind = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ArgKind::Choice(vec![]));
}

// ─── skip_serializing_if：None 字段不出现在 wire 上 ────────────────────────

#[test]
fn flag_short_none_skips_key_on_serialize() {
    let flag = FlagSpec {
        name: "force".into(),
        short: None,
        description: None,
    };
    let json = serde_json::to_string(&flag).unwrap();
    assert_eq!(json, r#"{"name":"force"}"#);
    // 反序列化仍能补全缺省字段。
    let back: FlagSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back, flag);
}

#[test]
fn arg_spec_description_none_skips_key_on_serialize() {
    let spec = ArgSpec {
        name: "query".into(),
        kind: ArgKind::String,
        required: false,
        description: None,
    };
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(json, r#"{"name":"query","required":false,"kind":"String"}"#);
}

// ─── snake_case 键名稳定（object 数组形态）────────────────────────────────

#[test]
fn serialized_keys_are_snake_case_and_stable() {
    let value = serde_json::to_value(full_schema()).unwrap();
    let obj = value.as_object().unwrap();
    // ArgsSchema 顶层：三个维度，键名 = 字段名（snake_case）。
    for key in ["positionals", "named", "flags"] {
        assert!(obj.contains_key(key), "缺少顶层键 {key}: {obj:?}");
    }
    // 数组元素为 object 形态（positional / named 共用 ArgSpec 形状）。
    let pos = obj["positionals"][0].as_object().unwrap();
    for key in ["name", "kind", "required", "description"] {
        assert!(pos.contains_key(key), "positional 缺键 {key}: {pos:?}");
    }
    let named = obj["named"][0].as_object().unwrap();
    for key in ["name", "kind", "required"] {
        assert!(named.contains_key(key), "named 缺键 {key}: {named:?}");
    }
    assert!(
        !named.contains_key("description"),
        "description=None 应被 skip: {named:?}"
    );
    let flag = obj["flags"][0].as_object().unwrap();
    for key in ["name", "short", "description"] {
        assert!(flag.contains_key(key), "flag 缺键 {key}: {flag:?}");
    }
    // kind 的 wire 形态（externally tagged）：String 为裸字符串，Choice 为对象。
    assert_eq!(pos["kind"], "String");
    assert_eq!(
        named["kind"],
        serde_json::json!({"Choice": ["claude", "gpt"]})
    );
    // short 为含连字符的展示形态（"-f"，计划步骤 3 形态定案）。
    assert_eq!(flag["short"], "-f");
}

// ─── ArgsSchema::parse 解析器（Phase 5 Step 6 交付）────────────────────────

/// rewind 形态 schema：required positional ×1 + presence-only flag ×1。
fn rewind_like_schema() -> ArgsSchema {
    ArgsSchema {
        positionals: vec![ArgSpec {
            name: "target_message_id".into(),
            kind: ArgKind::String,
            required: true,
            description: None,
        }],
        flags: vec![FlagSpec {
            name: "no-revert-files".into(),
            short: None,
            description: None,
        }],
        named: vec![],
    }
}

// ─── parse：rewind 形态（positional + presence-only flag）───────────────

#[test]
fn parse_rewind_args_positional_and_flag() {
    let parsed = rewind_like_schema()
        .parse("abc123 --no-revert-files")
        .unwrap();
    assert_eq!(parsed.positionals, vec!["abc123"]);
    assert!(parsed.named.is_empty());
    assert_eq!(parsed.flags, vec!["no-revert-files"]);
}

#[test]
fn parse_rewind_args_plain_positional() {
    let parsed = rewind_like_schema().parse("abc123").unwrap();
    assert_eq!(parsed.positionals, vec!["abc123"]);
    assert!(parsed.flags.is_empty());
}

/// 空参数 + required positional → 解析失败（拦截层据此返回 Error feedback，
/// 不进入 handler——rewind 现状语义泛化）。
#[test]
fn parse_rewind_empty_args_fails_required_positional() {
    let err = rewind_like_schema().parse("").unwrap_err();
    assert!(
        err.contains("target_message_id"),
        "错误应点名缺失参数: {err}"
    );
}

#[test]
fn parse_rewind_unknown_flag_fails() {
    let err = rewind_like_schema().parse("abc123 --bogus").unwrap_err();
    assert!(err.contains("unknown option"), "未知 flag 应报错: {err}");
}

#[test]
fn parse_rewind_extra_positional_fails() {
    let err = rewind_like_schema().parse("abc123 extra").unwrap_err();
    assert!(
        err.contains("unexpected argument"),
        "多余 positional 应报错: {err}"
    );
}

// ─── parse：完整形态（named Int/Choice 校验 + flag 长短双形态）─────────────

#[test]
fn parse_full_schema_named_and_flags() {
    let parsed = full_schema()
        .parse("hello --model gpt --depth 3 -f")
        .unwrap();
    assert_eq!(parsed.positionals, vec!["hello"]);
    assert_eq!(
        parsed.named,
        vec![
            ("model".to_string(), "gpt".to_string()),
            ("depth".to_string(), "3".to_string())
        ]
    );
    assert_eq!(parsed.flags, vec!["force"], "短形态 -f 应命中 force");
}

#[test]
fn parse_full_schema_missing_required_named_fails() {
    let err = full_schema().parse("hello").unwrap_err();
    assert!(err.contains("--depth"), "必填 named 缺失应报错: {err}");
}

#[test]
fn parse_full_schema_choice_out_of_candidates_fails() {
    let err = full_schema().parse("hello --model llama").unwrap_err();
    assert!(err.contains("候选"), "Choice 越界应报错: {err}");
}

#[test]
fn parse_full_schema_int_type_error_fails() {
    let err = full_schema()
        .parse("hello --model gpt --depth abc")
        .unwrap_err();
    assert!(err.contains("整数"), "Int 类型错误应报错: {err}");
}

#[test]
fn parse_full_schema_unknown_long_option_fails() {
    let err = full_schema().parse("hello --nope").unwrap_err();
    assert!(err.contains("--nope"), "未知长选项应报错: {err}");
}

// ─── parse：free-form（完全默认 schema，/bg 形态）──────────────────────────

#[test]
fn parse_default_schema_free_form_passes_through() {
    let schema = ArgsSchema::default();
    let parsed = schema.parse("任意 free-form 文本 --with-dashes").unwrap();
    assert_eq!(
        parsed.positionals,
        vec!["任意", "free-form", "文本", "--with-dashes"],
        "free-form 全部 token 原样归 positionals，零校验"
    );
    assert!(parsed.named.is_empty());
    assert!(parsed.flags.is_empty());
}

/// Path 参数校验存在性（设计 §73）。
#[test]
fn parse_path_arg_validates_existence() {
    let schema = ArgsSchema {
        positionals: vec![ArgSpec {
            name: "file".into(),
            kind: ArgKind::Path,
            required: true,
            description: None,
        }],
        named: vec![],
        flags: vec![],
    };
    assert!(schema.parse("/definitely/not/exists").is_err());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("existing.txt");
    std::fs::write(&path, "x").unwrap();
    let parsed = schema.parse(path.to_str().unwrap()).unwrap();
    assert_eq!(parsed.positionals, vec![path.to_str().unwrap().to_string()]);
}
