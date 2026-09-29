//! `workspace_resources` 契约测试：URI 构造/解析 round-trip、非法段拒绝矩阵、
//! plugin 组合规则、wire DTO 形状（camelCase / verbatim frontmatter / 省略语义）
//! 与 digest 格式。

use super::*;
use serde_json::json;

// ─── scope ───────────────────────────────────────────────────────────────────

#[test]
fn resource_scope_round_trips_and_rejects_unknown() {
    for scope in ResourceScope::ALL {
        assert_eq!(ResourceScope::parse(scope.as_str()), Some(scope));
    }
    for bogus in ["", "User", "local", "builtin ", "plugin:extra"] {
        assert_eq!(ResourceScope::parse(bogus), None, "未知 scope 必须拒绝");
    }
}

// ─── skill URI 构造 / 解析 ───────────────────────────────────────────────────

#[test]
fn skill_uri_round_trips_for_every_scope() {
    let cases = [
        (ResourceScope::User, None, "alpha"),
        (ResourceScope::Global, None, "alpha"),
        (ResourceScope::Project, None, "alpha"),
        (ResourceScope::Builtin, None, "alpha"),
        (ResourceScope::Plugin, Some("my-plugin"), "alpha"),
    ];
    for (scope, plugin, name) in cases {
        let uri = skill_uri(scope, plugin, name, SKILL_ENTRY_FILE)
            .unwrap_or_else(|| panic!("合法组合必须构造成功: {scope:?}"));
        let parsed = parse_skill_uri(&uri).unwrap_or_else(|| panic!("必须可解析: {uri}"));
        assert_eq!(parsed.scope, scope);
        assert_eq!(parsed.plugin_name.as_deref(), plugin);
        assert_eq!(parsed.name, name);
        assert_eq!(parsed.relative_path, SKILL_ENTRY_FILE);
    }
}

#[test]
fn skill_uri_keeps_colon_in_name_verbatim() {
    // X3：wire frontmatter 不改写——含 `:` 的名称原样进 URI（CLI 别名由宿主派生）。
    let uri = skill_uri(ResourceScope::User, None, "team:review", SKILL_ENTRY_FILE)
        .expect("含 `:` 的名称必须可构造");
    assert_eq!(uri, "skill://user/team:review/SKILL.md");
    let parsed = parse_skill_uri(&uri).expect("必须可解析");
    assert_eq!(parsed.name, "team:review");
}

#[test]
fn skill_uri_attachment_paths_are_relative_segments() {
    let uri = skill_uri(ResourceScope::User, None, "alpha", "scripts/run.sh")
        .expect("多层附件路径必须可构造");
    assert_eq!(uri, "skill://user/alpha/scripts/run.sh");
    let parsed = parse_skill_uri(&uri).expect("必须可解析");
    assert_eq!(parsed.relative_path, "scripts/run.sh");
}

#[test]
fn skill_uri_rejects_illegal_segments() {
    // 每例都是「构造拒绝」，按段形态分类（编码穿越 / NUL / 绝对路径 / 空段 / 控制字符）。
    let cases = [
        ("../escape", SKILL_ENTRY_FILE),
        (".", SKILL_ENTRY_FILE),
        ("..", SKILL_ENTRY_FILE),
        ("", SKILL_ENTRY_FILE),
        ("a/b", SKILL_ENTRY_FILE),
        ("a\\b", SKILL_ENTRY_FILE),
        ("a%2e%2e", SKILL_ENTRY_FILE),
        ("a?x", SKILL_ENTRY_FILE),
        ("a#x", SKILL_ENTRY_FILE),
        ("a\0b", SKILL_ENTRY_FILE),
        ("a\u{7}b", SKILL_ENTRY_FILE),
        ("alpha", "../SKILL.md"),
        ("alpha", "/SKILL.md"),
        ("alpha", "a//b"),
        ("alpha", "a/./b"),
        ("alpha", "a/../b"),
        ("alpha", "a%00b"),
        ("alpha", "a\\b"),
        ("alpha", "a\u{7}b"),
        ("alpha", ""),
    ];
    for (name, relative) in cases {
        assert!(
            skill_uri(ResourceScope::User, None, name, relative).is_none(),
            "非法组合必须拒绝: name={name:?} relative={relative:?}"
        );
    }
}

#[test]
fn skill_uri_enforces_plugin_combination_rules() {
    // plugin scope 必须带 plugin_name；其余 scope 必须不带。
    assert!(skill_uri(ResourceScope::Plugin, None, "alpha", SKILL_ENTRY_FILE).is_none());
    assert!(skill_uri(ResourceScope::User, Some("plug"), "alpha", SKILL_ENTRY_FILE).is_none());
    assert!(skill_uri(
        ResourceScope::Builtin,
        Some("plug"),
        "alpha",
        SKILL_ENTRY_FILE
    )
    .is_none());
    // plugin 名同样受段校验约束。
    assert!(skill_uri(
        ResourceScope::Plugin,
        Some("a/b"),
        "alpha",
        SKILL_ENTRY_FILE
    )
    .is_none());
    assert!(skill_uri(ResourceScope::Plugin, Some(".."), "alpha", SKILL_ENTRY_FILE).is_none());
}

#[test]
fn parse_skill_uri_rejects_foreign_shapes() {
    let bogus = [
        "",
        "skill:",
        "skill://",
        "skill://user",
        "skill://user/",
        "skill://user/alpha",
        "skill://user/alpha/",
        "skill://local/alpha/SKILL.md",
        "skill://plugin/alpha/SKILL.md",
        "agent://user/alpha/SKILL.md",
        "http://user/alpha/SKILL.md",
        "skill://user/../SKILL.md",
        "skill://plugin/a%2fb/alpha/SKILL.md",
        "skill://user/alpha/%2e%2e/x",
        "skill://user/alpha/SKILL.md?x=1",
        "skill://user/alpha/SKILL.md#frag",
    ];
    for uri in bogus {
        assert!(parse_skill_uri(uri).is_none(), "必须拒绝: {uri}");
    }
}

#[test]
fn parse_skill_uri_is_scheme_case_insensitive() {
    let parsed = parse_skill_uri("SKILL://user/alpha/SKILL.md").expect("scheme 大小写不敏感");
    assert_eq!(parsed.scope, ResourceScope::User);
    assert_eq!(parsed.name, "alpha");
}

// ─── agent URI ───────────────────────────────────────────────────────────────

#[test]
fn agent_uri_round_trips_and_pins_entry_file() {
    let uri = agent_uri(ResourceScope::Project, None, "coder").expect("合法组合");
    assert_eq!(uri, "agent://project/coder/agent.md");
    let parsed = parse_agent_uri(&uri).expect("必须可解析");
    assert_eq!(parsed.scope, ResourceScope::Project);
    assert_eq!(parsed.plugin_name, None);
    assert_eq!(parsed.name, "coder");

    let plugin_uri = agent_uri(ResourceScope::Plugin, Some("plug"), "coder").expect("合法组合");
    assert_eq!(plugin_uri, "agent://plugin/plug/coder/agent.md");
    let parsed = parse_agent_uri(&plugin_uri).expect("必须可解析");
    assert_eq!(parsed.plugin_name.as_deref(), Some("plug"));

    // entry 文件名固定：其他尾段不接受。
    assert!(parse_agent_uri("agent://user/coder/agent.txt").is_none());
    assert!(parse_agent_uri("agent://user/coder/extra/agent.md").is_none());
    assert!(parse_agent_uri("skill://user/coder/agent.md").is_none());
}

// ─── instruction URI ─────────────────────────────────────────────────────────

#[test]
fn instruction_uris_parse_to_documents() {
    for (uri, expected) in [
        (INSTRUCTION_MAIN_URI, InstructionDocument::Main),
        (INSTRUCTION_LOCAL_URI, InstructionDocument::Local),
        (INSTRUCTION_INDEX_URI, InstructionDocument::Index),
    ] {
        assert_eq!(parse_instruction_uri(uri), Some(expected));
        assert_eq!(expected.uri(), uri);
    }
}

#[test]
fn instruction_parser_rejects_foreign_shapes() {
    let bogus = [
        "peri-instruction://other/main",
        "peri-instruction://workspace/other",
        "peri-instruction://workspace/main/extra",
        "peri-instruction://workspace/",
        "peri-instruction:main",
        "redis://workspace/main",
    ];
    for uri in bogus {
        assert_eq!(parse_instruction_uri(uri), None, "必须拒绝: {uri}");
    }
}

// ─── digest ──────────────────────────────────────────────────────────────────

#[test]
fn digest_bytes_matches_known_sha256_vector() {
    // 空串与 "abc" 的 sha256 是公开测试向量（不是实现自证）。
    assert_eq!(
        digest_bytes(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        digest_bytes(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ─── wire 形状 ───────────────────────────────────────────────────────────────

#[test]
fn skills_list_response_serializes_camel_case_and_omits_absent_fields() {
    let entry = SkillEntry {
        uri: "skill://user/alpha/SKILL.md".to_string(),
        frontmatter: json!({
            "name": "alpha",
            "description": "demo",
            "custom-field": ["kept", "verbatim"]
        })
        .as_object()
        .expect("object")
        .clone(),
        resources: Some(vec![SkillResourceRef {
            uri: "skill://user/alpha/SKILL.md".to_string(),
            digest: digest_bytes(b"body"),
        }]),
    };
    let response = SkillsListResponse {
        skills: vec![entry],
        next_cursor: None,
        ttl_ms: None,
        cache_scope: None,
    };
    let value = serde_json::to_value(&response).expect("序列化");
    assert_eq!(
        value,
        json!({
            "skills": [{
                "uri": "skill://user/alpha/SKILL.md",
                "frontmatter": {
                    "name": "alpha",
                    "description": "demo",
                    "custom-field": ["kept", "verbatim"]
                },
                "resources": [{
                    "uri": "skill://user/alpha/SKILL.md",
                    "digest": digest_bytes(b"body")
                }]
            }]
        }),
        "wire 形状：camelCase、可选字段缺省省略、frontmatter 逐字保留未知字段"
    );

    // 反序列化回读（宿主侧解析路径等价物）。
    let parsed: SkillsListResponse = serde_json::from_value(value).expect("反序列化");
    assert_eq!(parsed, response);
}

#[test]
fn skills_get_response_round_trips_and_allows_missing_resources() {
    let response = SkillGetResponse {
        skill: SkillEntry {
            uri: "skill://project/beta/SKILL.md".to_string(),
            frontmatter: json!({"name": "beta", "description": "d"})
                .as_object()
                .unwrap()
                .clone(),
            resources: None,
        },
    };
    let value = serde_json::to_value(&response).expect("序列化");
    assert_eq!(
        value,
        json!({
            "skill": {
                "uri": "skill://project/beta/SKILL.md",
                "frontmatter": {"name": "beta", "description": "d"}
            }
        }),
        "skills/get 的 skill 字段与 list 条目同构；resources 缺省省略"
    );
    let parsed: SkillGetResponse = serde_json::from_value(value).expect("反序列化");
    assert_eq!(parsed, response);
}

#[test]
fn empty_skills_list_serializes_as_empty_array() {
    let value = serde_json::to_value(SkillsListResponse {
        skills: Vec::new(),
        next_cursor: None,
        ttl_ms: None,
        cache_scope: None,
    })
    .expect("序列化");
    assert_eq!(value, json!({"skills": []}));
}
