use super::*;
use serde_json::json;

// ─── display_name：分级展示（「权威词法」）───────────────────────────────

#[test]
fn display_name_level1_uses_bare_name_after_last_colon() {
    assert_eq!(display_name("core:compact", 1), "compact");
    assert_eq!(display_name("ui:history", 1), "history");
    // 多段全名取最右段
    assert_eq!(display_name("demo:hello", 1), "hello");
}

#[test]
fn display_name_level2_keeps_fullname() {
    assert_eq!(display_name("core:compact", 2), "core:compact");
    assert_eq!(display_name("demo:hello", 2), "demo:hello");
    assert_eq!(display_name("plaincmd", 2), "plaincmd");
}

#[test]
fn display_name_without_colon_falls_back_to_fullname() {
    // level 1 但全名无冒号（防御：裸名条目）——原样返回
    assert_eq!(display_name("plaincmd", 1), "plaincmd");
    // 未知 level 一律全名原样
    assert_eq!(display_name("core:compact", 0), "core:compact");
    assert_eq!(display_name("core:compact", 3), "core:compact");
}

// ─── parse_projection_kind：wire snake_case → 渲染枚举 ────────────────────

#[test]
fn parse_projection_kind_all_variants() {
    assert_eq!(
        parse_projection_kind("command"),
        Some(SlashActionKind::Command)
    );
    assert_eq!(parse_projection_kind("skill"), Some(SlashActionKind::Skill));
    assert_eq!(
        parse_projection_kind("mcp_skill"),
        Some(SlashActionKind::McpSkill)
    );
    assert_eq!(parse_projection_kind("panel"), Some(SlashActionKind::Panel));
}

#[test]
fn parse_projection_kind_unknown_returns_none() {
    assert_eq!(parse_projection_kind(""), None);
    assert_eq!(parse_projection_kind("unknown"), None);
    // 大小写敏感：wire 是 Phase 1 snake_case 锁定形态，大写/驼峰不命中
    assert_eq!(parse_projection_kind("Command"), None);
    assert_eq!(parse_projection_kind("mcpSkill"), None);
    // 缺失 key 语义 = 调用方缺省回退 Command（None 不 panic）
    assert_eq!(parse_projection_kind("__missing__"), None);
}

// ─── ArgsSchema：wire 往返（flags 为 object 数组的 FlagSpec，P1-5）───────

#[test]
fn args_schema_roundtrips_from_wire_flags_object_array() {
    // 镜像 Phase 1 peri-acp-types serde 模型：flags 元素是 object（FlagSpec），
    // 不是字符串数组——字符串数组反序列化必然失败。
    let wire = json!({
        "positionals": [
            { "name": "query", "kind": "String", "required": true,
              "description": "搜索关键词" }
        ],
        "named": [
            { "name": "limit", "kind": "Int", "required": false }
        ],
        "flags": [
            { "name": "--force", "short": "-f", "description": "强制执行" },
            { "name": "--verbose", "description": "详细输出" }
        ]
    });
    let schema: ArgsSchema = serde_json::from_value(wire).expect("wire 形态应可反序列化");

    assert_eq!(schema.positionals.len(), 1);
    assert_eq!(schema.positionals[0].name, "query");
    assert_eq!(schema.positionals[0].kind, ArgKind::String);
    assert!(schema.positionals[0].required);
    assert_eq!(
        schema.positionals[0].description.as_deref(),
        Some("搜索关键词")
    );

    assert_eq!(schema.named.len(), 1);
    assert_eq!(schema.named[0].name, "limit");
    assert_eq!(schema.named[0].kind, ArgKind::Int);
    assert!(!schema.named[0].required);

    assert_eq!(schema.flags.len(), 2);
    assert_eq!(schema.flags[0].name, "--force");
    assert_eq!(schema.flags[0].short.as_deref(), Some("-f"));
    assert_eq!(schema.flags[1].short, None);
}

#[test]
fn args_schema_roundtrips_choice_and_path_kinds() {
    let wire = json!({
        "positionals": [
            { "name": "mode", "kind": { "Choice": ["fast", "safe"] } },
            { "name": "file", "kind": "Path" }
        ]
    });
    let schema: ArgsSchema = serde_json::from_value(wire).unwrap();
    assert_eq!(
        schema.positionals[0].kind,
        ArgKind::Choice(vec!["fast".to_string(), "safe".to_string()])
    );
    assert_eq!(schema.positionals[1].kind, ArgKind::Path);

    // 序列化往返保持 wire 形态（externally tagged，变体名原样）
    let back = serde_json::to_value(&schema).unwrap();
    assert_eq!(
        back["positionals"][0]["kind"],
        json!({ "Choice": ["fast", "safe"] })
    );
    assert_eq!(back["positionals"][1]["kind"], json!("Path"));
}

#[test]
fn args_schema_missing_fields_default_to_empty() {
    // 旧投影不含 args 字段 / 维度缺省 → 反序列化即得全默认（serde default）
    let schema: ArgsSchema = serde_json::from_value(json!({})).unwrap();
    assert_eq!(schema, ArgsSchema::default());
}

// ─── SlashCommandEntry：缺省形态与 Default 语义 ──────────────────────────

#[test]
fn slash_command_entry_default_is_metadata_free_fallback() {
    // 无 `_meta` 投影的条目：kind=Command / level=1 / 其余空——与
    // acp_notifier 缺省回退语义一致（R1 分类退化下限）。
    let entry = SlashCommandEntry {
        fullname: "core:compact".into(),
        description: "紧凑模式".into(),
        ..Default::default()
    };
    assert_eq!(entry.kind, SlashActionKind::Command);
    assert_eq!(entry.level, 1);
    assert!(entry.aliases.is_empty());
    assert_eq!(entry.category, None);
    assert_eq!(entry.args, None);
}
