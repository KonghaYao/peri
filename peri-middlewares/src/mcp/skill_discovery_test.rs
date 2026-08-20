//! 发现测试：legacy（select/parse/collect 纯函数 + run_discovery 级）与
//! SEP-2640 规范路径（skills/list 解析、digest 校验、frontmatter 比对、
//! 同名消歧、嵌套过滤、端到端 run_discovery）。

use super::*;
use crate::mcp::client::{McpClientHandle, OAuthStatus};
use crate::mcp::ClientStatus;
use peri_acp_types::mcp_skills::ServerDiscoveryState;
use std::sync::Arc;

fn resource(uri: &str) -> Resource {
    Resource::new(uri.to_string(), "desc".to_string())
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ─── select_skill_resources ────────────────────────────────────────────────

#[test]
fn select_skill_resources_filters_to_skill_md() {
    let resources = vec![
        resource("skill://demo/SKILL.md"),
        resource("skill://other/sub/SKILL.md"),
        // 附属资源：同前缀非 SKILL.md → 过滤
        resource("skill://demo/notes/README.md"),
        resource("skill://demo/scripts/run.sh"),
        // 非 skill:// 前缀 → 过滤
        resource("https://example.com/skill://demo/SKILL.md"),
        resource("file:///skill.md"),
        // 非 /SKILL.md 后缀 → 过滤
        resource("skill://demo/SKILL.md.bak"),
        resource("skill://demo/skill.md"),
        // scheme 大小写不敏感（RFC 3986）：SKILL:// / Skill:// 均命中
        resource("SKILL://demo/SKILL.md"),
        resource("Skill://demo/SKILL.md"),
    ];
    let selected = select_skill_resources(&resources);
    let uris: Vec<&str> = selected.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec![
            "skill://demo/SKILL.md",
            "skill://other/sub/SKILL.md",
            "SKILL://demo/SKILL.md",
            "Skill://demo/SKILL.md",
        ],
        "仅 skill:// 前缀（大小写不敏感）+ /SKILL.md 后缀入选"
    );
}

#[test]
fn select_skill_resources_empty() {
    assert!(select_skill_resources(&[]).is_empty());
    assert!(select_skill_resources(&[resource("https://x/SKILL.md")]).is_empty());
}

// ─── filter_nested_skills ──────────────────────────────────────────────────

#[test]
fn filter_nested_skills_keeps_top_level_only() {
    let resources = vec![
        resource("skill://root/SKILL.md"),
        resource("skill://root/sub/SKILL.md"), // 嵌套：root 是技能目录
        resource("skill://other/sub/SKILL.md"), // 非嵌套：无 skill://other/SKILL.md
    ];
    let kept = filter_nested_skills(resources);
    let uris: Vec<&str> = kept.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec!["skill://root/SKILL.md", "skill://other/sub/SKILL.md"],
        "嵌套 SKILL.md（祖先路径有另一技能）不单独注册；带前缀的顶层技能保留"
    );
}

// ─── parse_mcp_skill_md（legacy 身份：最终段 = frontmatter name）──────────

const SAMPLE_MD: &str = "---\nname: demo-skill\ndescription: Say hello\n---\n\n# Hello\n";

#[test]
fn parse_ok_builds_metadata() {
    let meta =
        parse_mcp_skill_md(SAMPLE_MD, "demo", "skill://demo-skill/SKILL.md").expect("应解析成功");
    assert_eq!(
        meta.name,
        mcp_skill_name("demo", "demo-skill"),
        "注册名来自 frontmatter name（须与 uri 最终段一致）"
    );
    assert_eq!(meta.description, "Say hello");
    assert_eq!(meta.path, PathBuf::new());
    assert_eq!(meta.source, SkillSource::Mcp);
    assert_eq!(meta.plugin_name, None);
    assert_eq!(
        meta.origin,
        Some(SkillOrigin::Mcp {
            server: "demo".to_string(),
            uri: "skill://demo-skill/SKILL.md".to_string(),
        })
    );
    assert_eq!(meta.content.as_deref(), Some(SAMPLE_MD), "content 存全文");
}

#[test]
fn parse_missing_name_returns_none() {
    let content = "---\ndescription: no name here\n---\n\n# Body\n";
    assert!(
        parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 name → None"
    );
}

#[test]
fn parse_missing_description_returns_none() {
    let content = "---\nname: orphan\n---\n\n# Body\n";
    assert!(
        parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 description → None"
    );
}

#[test]
fn parse_invalid_yaml_returns_none() {
    assert!(parse_mcp_skill_md("not: [valid\nyaml", "srv", "skill://srv/SKILL.md").is_none());
    assert!(
        parse_mcp_skill_md("# 无 frontmatter\n\n正文", "srv", "skill://srv/SKILL.md").is_none()
    );
}

#[test]
fn parse_uri_final_segment_is_name_and_must_match_frontmatter() {
    // 带前缀：最终段 = sub（前缀 ns 是组织前缀，不入注册名）
    let content = "---\nname: sub\ndescription: d\n---\n";
    let meta = parse_mcp_skill_md(content, "srv", "skill://ns/sub/SKILL.md").unwrap();
    assert_eq!(meta.name, "mcp__srv__sub");

    // 非法字符（空格/点/β）sanitize 后比对一致 → 接受
    let content2 = "---\nname: v1.0-β\ndescription: d\n---\n";
    let meta2 = parse_mcp_skill_md(content2, "srv", "skill://my skill/v1.0-β/SKILL.md").unwrap();
    assert_eq!(meta2.name, "mcp__srv__v1_0-_");

    // 最终段与 frontmatter name 不一致 → 拒绝（身份必须可验证，防注入）
    let meta3 = parse_mcp_skill_md(
        "---\nname: other\ndescription: d\n---\n",
        "srv",
        "skill://a/SKILL.md",
    );
    assert!(meta3.is_none(), "uri 最终段 != frontmatter name → None");
}

#[test]
fn parse_uri_without_skill_prefix_or_suffix_returns_none() {
    assert!(
        parse_mcp_skill_md(SAMPLE_MD, "srv", "https://x/SKILL.md").is_none(),
        "非 skill:// 前缀 → None"
    );
    assert!(
        parse_mcp_skill_md(SAMPLE_MD, "srv", "skill://srv/other.md").is_none(),
        "非 /SKILL.md 后缀 → None"
    );
}

/// scheme 大小写不敏感（RFC 3986）：SKILL:// 前缀与 skill:// 同义。
#[test]
fn parse_uri_skill_scheme_case_insensitive() {
    let meta = parse_mcp_skill_md(SAMPLE_MD, "srv", "SKILL://demo-skill/SKILL.md").unwrap();
    assert_eq!(meta.name, "mcp__srv__demo-skill", "大写 scheme 应同样解析");
    let meta2 = parse_mcp_skill_md(SAMPLE_MD, "srv", "Skill://ns/demo-skill/SKILL.md").unwrap();
    assert_eq!(
        meta2.name, "mcp__srv__demo-skill",
        "混合大小写 scheme 应同样解析"
    );
}

#[test]
fn parse_description_trimmed() {
    // YAML 折叠标量尾部可能带 \n，与 loader.rs 一致做 trim
    let content = "---\nname: d\ndescription: >\n  hello\n  world\n---\n\n# Body\n";
    let meta = parse_mcp_skill_md(content, "srv", "skill://d/SKILL.md").unwrap();
    assert_eq!(meta.description, "hello world");
}

// ─── disambiguate_names ────────────────────────────────────────────────────

#[test]
fn disambiguate_names_on_collision_uses_path_segments() {
    let mk = |name: &str, uri: &str| SkillMetadata {
        name: name.to_string(),
        description: String::new(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: uri.to_string(),
        }),
        content: None,
        // 消歧只依赖 name/origin，resources 不参与
        resources: Vec::new(),
    };
    let entries = vec![
        mk("mcp__srv__refunds", "skill://acme/billing/refunds/SKILL.md"),
        mk("mcp__srv__refunds", "skill://acme/support/refunds/SKILL.md"),
        mk("mcp__srv__unique", "skill://unique/SKILL.md"),
    ];
    let out = disambiguate_names("srv", entries);
    let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "mcp__srv__acme_billing_refunds",
            "mcp__srv__acme_support_refunds",
            "mcp__srv__unique",
        ],
        "同名冲突用完整路径段消歧（两个都保留），唯一名不动"
    );
}

// ─── SEP-2640 规范路径纯函数 ──────────────────────────────────────────────

#[test]
fn verify_digest_matches_sha256() {
    let hex = sha256_hex(SAMPLE_MD);
    assert!(verify_digest(SAMPLE_MD, &format!("sha256:{hex}")));
    assert!(!verify_digest(
        SAMPLE_MD,
        &format!("sha256:{}", "0".repeat(64))
    ));
    assert!(
        !verify_digest(SAMPLE_MD, &format!("SHA256:{hex}")),
        "前缀必须小写 sha256:"
    );
    assert!(!verify_digest(SAMPLE_MD, "md5:abc"));
    assert!(!verify_digest(SAMPLE_MD, &hex), "缺 sha256: 前缀");
    assert!(!verify_digest(SAMPLE_MD, "sha256:short"));
    // 大写 hex 通过（互操作宽容：server 生成大写 hex digest 是合法场景，
    // 校验前统一转小写比较）
    let upper = hex.to_uppercase();
    assert!(
        verify_digest(SAMPLE_MD, &format!("sha256:{upper}")),
        "大写 hex digest 应通过（互操作宽容）"
    );
    // 非 hex 字符拒绝（is_ascii_hexdigit 门闩）
    assert!(
        !verify_digest(SAMPLE_MD, &format!("sha256:{}", "g".repeat(64))),
        "g 非 hex 字符 → 拒绝"
    );
    assert!(
        !verify_digest(SAMPLE_MD, &format!("sha256:{}", "z".repeat(64))),
        "z 非 hex 字符 → 拒绝"
    );
}

#[test]
fn entry_from_dto_extracts_name_description() {
    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": "A skill", "license": "MIT" },
        "resources": [{ "uri": "skill://a/SKILL.md", "digest": "sha256:abc" }],
    }))
    .unwrap();
    let entry = entry_from_dto(dto).unwrap();
    assert_eq!(entry.uri, "skill://a/SKILL.md");
    // name/description 门闩通过后，frontmatter 原样保留（verbatim，逐字段
    // 比对用）——附加字段 license 也在其中
    assert_eq!(
        entry.frontmatter,
        serde_json::json!({ "name": "a", "description": "A skill", "license": "MIT" })
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        entry.resources,
        Some(vec![SkillResource {
            uri: "skill://a/SKILL.md".to_string(),
            digest: "sha256:abc".to_string(),
        }])
    );
}

#[test]
fn entry_from_dto_resources_omitted_vs_empty_distinguished() {
    // 省略 → None（动态生成技能，规范 MAY）
    let omitted: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": "A" },
    }))
    .unwrap();
    let entry = entry_from_dto(omitted).unwrap();
    assert_eq!(entry.resources, None, "省略 resources → None（动态技能）");

    // 显式空数组 → Some(空)（present 但完整性违规，由 verify_and_build 拒绝）
    let empty: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": "A" },
        "resources": [],
    }))
    .unwrap();
    let entry = entry_from_dto(empty).unwrap();
    assert_eq!(
        entry.resources,
        Some(vec![]),
        "显式空数组 → Some(空)（与省略区分）"
    );

    // 非空 → Some(非空)
    let full: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": "A" },
        "resources": [{ "uri": "skill://a/SKILL.md", "digest": "sha256:abc" }],
    }))
    .unwrap();
    let entry = entry_from_dto(full).unwrap();
    assert_eq!(entry.resources.as_ref().unwrap().len(), 1);
}

#[test]
fn entry_from_dto_missing_name_returns_none() {
    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "description": "no name" },
    }))
    .unwrap();
    assert!(entry_from_dto(dto).is_none());
}

#[test]
fn skill_list_response_parses_pagination_and_defaults() {
    let json = serde_json::json!({
        "skills": [
            { "uri": "skill://a/SKILL.md", "frontmatter": { "name": "a", "description": "A" } },
            {
                "uri": "skill://b/SKILL.md",
                "frontmatter": { "name": "b", "description": "B" },
                "resources": [{ "uri": "skill://b/SKILL.md", "digest": "sha256:abcd" }]
            }
        ],
        "nextCursor": "page-2"
    });
    let parsed: SkillListResponse = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.next_cursor.as_deref(), Some("page-2"));
    assert_eq!(parsed.skills.len(), 2);
    // resources 可省略（动态生成技能）：缺省 → None
    let e0 = entry_from_dto(parsed.skills[0].clone()).unwrap();
    assert!(e0.resources.is_none(), "省略 resources → None");
    let e1 = entry_from_dto(parsed.skills[1].clone()).unwrap();
    assert_eq!(e1.resources.as_ref().unwrap().len(), 1);
}

// ─── verify_and_build（digest / frontmatter / uri 段校验）──────────────────

const SPEC_MD: &str = "---\nname: a\ndescription: A skill\n---\n\n# A\n";

fn spec_entry(digest: Option<&str>) -> SkillListEntry {
    let digest = match digest {
        Some(d) => d.to_string(),
        None => format!("sha256:{}", sha256_hex(SPEC_MD)),
    };
    SkillListEntry {
        uri: "skill://a/SKILL.md".to_string(),
        frontmatter: serde_json::json!({ "name": "a", "description": "A skill" })
            .as_object()
            .unwrap()
            .clone(),
        resources: Some(vec![SkillResource {
            uri: "skill://a/SKILL.md".to_string(),
            digest,
        }]),
    }
}

#[test]
fn verify_and_build_ok() {
    let outcome = verify_and_build("srv", &spec_entry(None), SPEC_MD);
    let VerifyOutcome::Built(meta) = outcome else {
        panic!("digest+frontmatter 一致应构建，实际: {outcome:?}");
    };
    assert_eq!(meta.name, "mcp__srv__a");
    assert_eq!(meta.description, "A skill");
    assert_eq!(
        meta.origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://a/SKILL.md".to_string(),
        })
    );
}

#[test]
fn verify_and_build_digest_mismatch_rejected() {
    let entry = spec_entry(Some(&format!("sha256:{}", "0".repeat(64))));
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::DigestMismatch
        ),
        "digest 不匹配（内容被替换/陈旧）→ DigestMismatch（stale 信号）"
    );
}

#[test]
fn verify_and_build_frontmatter_mismatch_rejected() {
    let mut entry = spec_entry(None);
    entry
        .frontmatter
        .insert("description".to_string(), "Stale description".into());
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::Rejected
        ),
        "读到的 frontmatter 与条目不一致 → 拒绝"
    );
}

#[test]
fn verify_and_build_uri_name_mismatch_rejected() {
    let mut entry = spec_entry(None);
    entry.uri = "skill://b/SKILL.md".to_string();
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::Rejected
        ),
        "uri 最终段 != frontmatter name → 拒绝"
    );
}

#[test]
fn verify_and_build_without_resources_accepted() {
    let mut entry = spec_entry(None);
    entry.resources = None;
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::Built(_)
        ),
        "省略 resources（None，动态生成技能）：接受但不可内容绑定（规范 MAY 省略）"
    );
}

/// resources present 但为显式空数组 → 完整性违规（present 时必须完整，
/// 含 SKILL.md 自身条目）→ 拒绝。
#[test]
fn verify_and_build_empty_resources_rejected() {
    let mut entry = spec_entry(None);
    entry.resources = Some(vec![]);
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::Rejected
        ),
        "显式空 resources → 完整性违规拒绝"
    );
}

/// resources present 但未含 SKILL.md 自身条目（只列了附属资源）→ 拒绝。
#[test]
fn verify_and_build_resources_missing_self_rejected() {
    let mut entry = spec_entry(None);
    entry.resources = Some(vec![SkillResource {
        uri: "skill://a/notes.md".to_string(),
        digest: format!("sha256:{}", "0".repeat(64)),
    }]);
    assert!(
        matches!(
            verify_and_build("srv", &entry, SPEC_MD),
            VerifyOutcome::Rejected
        ),
        "resources 未含 SKILL.md 自身条目 → 完整性违规拒绝"
    );
}

// ─── frontmatter 逐字段全量比对（任务：附加字段/值类型归一）────────────────

/// entry frontmatter 含附加字段 license；内容侧 license 被改 → 拒绝
/// （附加字段差异也是验证失败，规范 MUST NOT load）。
#[test]
fn verify_and_build_extra_field_mismatch_rejected() {
    let content = "---\nname: a\ndescription: A skill\nlicense: Apache-2.0\n---\n\n# A\n";
    // content 与 entry frontmatter 的 name/description 一致、digest 也匹配，
    // 但 license 不同 → 全量比对失败
    let mut entry = spec_entry(Some(&spec_entry_digest_for(content)));
    entry
        .frontmatter
        .insert("license".to_string(), "MIT".into());
    assert!(
        matches!(
            verify_and_build("srv", &entry, content),
            VerifyOutcome::Rejected
        ),
        "附加字段 license 不一致 → 拒绝"
    );
}

/// 附加字段全等（含嵌套 metadata 对象）→ 接受。
#[test]
fn verify_and_build_extra_fields_equal_accepted() {
    let content = "---\nname: a\ndescription: A skill\nlicense: MIT\nmetadata:\n  author: x\n  tags: [t1, t2]\n---\n\n# A\n";
    let mut entry = spec_entry(Some(&spec_entry_digest_for(content)));
    entry.frontmatter = serde_json::json!({
        "name": "a",
        "description": "A skill",
        "license": "MIT",
        "metadata": { "author": "x", "tags": ["t1", "t2"] },
    })
    .as_object()
    .unwrap()
    .clone();
    assert!(
        matches!(
            verify_and_build("srv", &entry, content),
            VerifyOutcome::Built(_)
        ),
        "附加字段全等（含嵌套对象/数组）→ 接受"
    );
}

/// YAML 值类型严格比较（2026-08-15 定案）：数字 ↔ 字符串**跨类型不相等**
/// （42 ≠ "42"，规范 "identical in content" 字面；两侧类型不一致即内容
/// 差异）→ 拒绝。
#[test]
fn verify_and_build_yaml_value_type_cross_type_rejected() {
    let content = "---\nname: a\ndescription: A skill\nversion: 42\n---\n\n# A\n";
    let mut entry = spec_entry(Some(&spec_entry_digest_for(content)));
    entry.frontmatter.insert("version".to_string(), "42".into());
    assert!(
        matches!(
            verify_and_build("srv", &entry, content),
            VerifyOutcome::Rejected
        ),
        "YAML 数字 42 与字符串 \"42\" 跨类型 → 拒绝"
    );
}

/// YAML 尾随空白归一：String vs String 比较前两侧 trim_end（block scalar
/// 渲染差异容忍）→ 接受。
#[test]
fn verify_and_build_yaml_trailing_whitespace_normalized() {
    let content = "---\nname: a\ndescription: A skill\nversion: |\n  1.0\n---\n\n# A\n";
    let mut entry = spec_entry(Some(&spec_entry_digest_for(content)));
    // 条目侧 version 为 "1.0\n"（渲染差异），内容侧为 "1.0\n"（block scalar
    // 自带换行）——trim_end 后相等。
    entry
        .frontmatter
        .insert("version".to_string(), "1.0".into());
    assert!(
        matches!(
            verify_and_build("srv", &entry, content),
            VerifyOutcome::Built(_)
        ),
        "字符串尾随空白归一（trim_end）→ 接受"
    );
}

/// 计算 content 的 sha256 digest（spec_entry 用）。
fn spec_entry_digest_for(content: &str) -> String {
    format!("sha256:{}", sha256_hex(content))
}

/// frontmatter_maps_equal 纯函数单测：键集合/值类型归一/嵌套递归。
#[test]
fn frontmatter_maps_equal_units() {
    use serde_json::{json, Map, Value};
    let map = |v: Value| v.as_object().unwrap().clone();
    // 全等（含嵌套）
    let a = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] }, "license": "MIT" }));
    let b = map(json!({ "license": "MIT", "meta": { "tags": ["t1", 2] }, "name": "a" }));
    assert!(frontmatter_maps_equal(&a, &b), "键序无关的全等");
    // 附加字段差异 → 不相等
    let c = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] }, "license": "Apache" }));
    assert!(!frontmatter_maps_equal(&a, &c), "license 差异 → 不等");
    // 键缺失 → 不相等
    let d = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] } }));
    assert!(!frontmatter_maps_equal(&a, &d), "缺 license 键 → 不等");
    // 数字 ↔ 字符串跨类型严格化（2026-08-15 定案）：42 ≠ "42"
    assert!(!frontmatter_values_equal(&json!("42"), &json!(42u64)));
    assert!(!frontmatter_values_equal(&json!(42u64), &json!("42")));
    assert!(!frontmatter_values_equal(&json!(1.0f64), &json!("1")));
    assert!(!frontmatter_values_equal(&json!("42"), &json!(43u64)));
    assert!(!frontmatter_values_equal(&json!("abc"), &json!(42u64)));
    // Number vs Number 保留 serde_json 混合 f64 比较（1 == 1.0 成立）
    assert!(frontmatter_values_equal(&json!(1u64), &json!(1.0f64)));
    assert!(frontmatter_values_equal(&json!(42u64), &json!(42u64)));
    // String vs String 尾随空白归一（仅 trim_end，不做 trim 全量）
    assert!(frontmatter_values_equal(&json!("hello\n"), &json!("hello")));
    assert!(!frontmatter_values_equal(&json!(" hello"), &json!("hello")));
    // 嵌套数组内跨类型 → 不等（与顶层同规则）
    let e = map(json!({ "n": [1, "2"] }));
    let f = map(json!({ "n": ["1", 2] }));
    assert!(!frontmatter_maps_equal(&e, &f), "嵌套数组跨类型 → 不等");
    // 跨类型（bool vs 字符串）→ 不等
    assert!(!frontmatter_values_equal(&json!(true), &json!("true")));
    // 空 map 全等
    let empty: Map<String, Value> = Map::new();
    assert!(frontmatter_maps_equal(&empty, &empty));
}

/// number_eq 大数精度（2026-08-15 第三轮 review）：f64 兜底对 >2^53 的
/// Float-vs-Int 会因舍入误判相等。Float 绝对值 ≤ 2^53 → 转整数精确比较；
/// 域外 → 保守判不等。
///
/// 注：任务原案 `json!(9007199254740993f64)` 在 Rust 中字面量即舍入为
/// 9007199254740992.0（2^53+1 不可精确表示），与 `9007199254740992f64`
/// 无法区分；故域内精确比较用相邻大整数（案例 2——旧代码 f64 兜底误判
/// true 的等价场景），域外样本用 2^53+2（域外第一个可表示值）。
#[test]
fn number_eq_large_integer_precision() {
    use serde_json::json;
    // 域内边界（2^53 可精确表示）：Float 与 Int 精确相等
    assert!(frontmatter_values_equal(
        &json!(9007199254740992f64),
        &json!(9007199254740992u64)
    ));
    // 域内精确比较：相邻大整数不被 f64 舍入糊掉（旧代码：
    // 9007199254740993u64 as f64 舍入为 9007199254740992.0 → 误判相等）
    assert!(!frontmatter_values_equal(
        &json!(9007199254740992f64),
        &json!(9007199254740993u64)
    ));
    // 域外保守拒绝：2^53+2 与同名整数 f64 表示相同，但超出 f64 精确整数域
    // → 判不等（旧代码误判相等）
    assert!(!frontmatter_values_equal(
        &json!(9007199254740994f64),
        &json!(9007199254740994u64)
    ));
    // 负域：-2^53 边界精确相等；-2^53-2 域外保守拒绝
    assert!(frontmatter_values_equal(
        &json!(-9007199254740992f64),
        &json!(-9007199254740992i64)
    ));
    assert!(!frontmatter_values_equal(
        &json!(-9007199254740994f64),
        &json!(-9007199254740994i64)
    ));
    // 域内小整数回归：1 == 1.0 仍成立；非整数 Float 不与整数相等
    assert!(frontmatter_values_equal(&json!(1u64), &json!(1.0f64)));
    assert!(!frontmatter_values_equal(&json!(1u64), &json!(1.5f64)));
}

// ─── collect_skill_entries 排序（legacy：每请求独立 spawn 服务器）─────────

/// 单请求响应规则：uri 命中 `segment` 子串时应用；`delay` 为响应前延迟；
/// `error` 为 true 时返回 JSON-RPC error（模拟 read 失败）。
#[derive(Clone)]
struct RespondRule {
    segment: &'static str,
    delay: std::time::Duration,
    error: bool,
}

/// 原始 JSON-RPC responder（仅用 client feature，不引入 rmcp server 面）：
/// 逐行读请求，**每请求独立 spawn** 响应任务（并发写经 Mutex<WriteHalf>
/// 串行化，消息边界安全）；`first_done` 非 None 时在首个响应写出后 notify
/// （cancel 时序同步用）；`completion_log` 非 None 时按写出顺序记录 uri 段
/// （锁定完成序，日志先于响应写出保证可见性）。
///
/// read 响应的 frontmatter name = uri 最后一段（与资源过滤语义对齐：
/// 最终段即技能名）。
async fn raw_skill_server(
    io: tokio::io::DuplexStream,
    rules: Vec<RespondRule>,
    first_done: Option<Arc<tokio::sync::Notify>>,
    completion_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            line.clear();
            continue;
        };
        line.clear();
        let writer = Arc::clone(&writer);
        let first_done = first_done.clone();
        let completion_log = completion_log.clone();
        let rules = rules.clone();
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let rule = rules.iter().find(|r| uri.contains(r.segment)).cloned();
            if let Some(r) = &rule {
                if !r.delay.is_zero() {
                    tokio::time::sleep(r.delay).await;
                }
            }
            let response = if rule.as_ref().map(|r| r.error).unwrap_or(false) {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32000, "message": "read failed" }
                })
            } else {
                let segment = uri
                    .strip_prefix("skill://")
                    .and_then(|u| u.strip_suffix("/SKILL.md"))
                    .and_then(|u| u.rsplit('/').next())
                    .unwrap_or("unknown");
                let text = format!(
                    "---\nname: {segment}\ndescription: desc for {segment}\n---\n\n# Body\n"
                );
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "contents": [{ "uri": uri, "mimeType": "text/plain", "text": text }]
                    }
                })
            };
            if let Some(log) = &completion_log {
                log.lock().unwrap().push(uri.clone());
            }
            let mut w = writer.lock().await;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            drop(w);
            if let Some(n) = &first_done {
                n.notify_one();
            }
        });
    }
}

#[tokio::test]
async fn collect_skill_entries_sorts_by_name_despite_completion_order() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    // 服务端每请求独立 spawn：zebra 立即返回、alpha 延迟 200ms → 完成序
    // 确定性为 [zebra, alpha]（与 name 排序相反）；若 collect 不排序，
    // entries 将保持完成序。
    let completion_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_millis(200),
            error: false,
        }],
        None,
        Some(Arc::clone(&completion_log)),
    ));

    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer().clone();

    let resources = vec![
        resource("skill://srv/zebra/SKILL.md"),
        resource("skill://srv/alpha/SKILL.md"),
    ];
    let cancel = AgentCancellationToken::new();
    let (_, entries) = collect_skill_entries(peer, "srv", resources, cancel).await;

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // frontmatter name = uri 最终段（zebra/alpha）→ 注册名
    assert_eq!(
        names,
        vec!["mcp__srv__alpha", "mcp__srv__zebra"],
        "JoinSet 完成序非确定，输出应按 name 排序"
    );
    // 完成序确定性：zebra（无延迟）先于 alpha（200ms 延迟）写出——与排序序
    // 相反，证明排序断言确实覆盖了乱序输入。
    assert_eq!(
        *completion_log.lock().unwrap(),
        vec![
            "skill://srv/zebra/SKILL.md".to_string(),
            "skill://srv/alpha/SKILL.md".to_string()
        ],
        "服务端完成序应为 [zebra, alpha]，与排序序相反"
    );
}

// ─── run_discovery 级测试（legacy 路径）───────────────────────────────────

/// 极简 tracing Subscriber：只捕获 WARN 事件的 message 字段（不引入
/// tracing-subscriber dev-dependency；tracing 根导出全套 Subscriber/Visit）。
struct WarnCaptureSubscriber {
    warns: Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(0)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct MessageVisitor(String);
        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.warns.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// 构造 discovery 用 McpClientHandle（peer 已连接 duplex 客户端面；
/// legacy：skills_capable=false）。
fn make_discovery_handle(
    running: &rmcp::service::RunningService<RoleClient, ()>,
    resources: Vec<Resource>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: "srv".to_string(),
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

/// candidates 非空 + 全部 read 失败 → 汇总 warn（回写空条目 Discovered）。
#[test]
fn run_discovery_all_reads_fail_emits_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(3u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                // 空段规则匹配所有 uri → 全部返回 JSON-RPC error（read 失败）
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![RespondRule {
                        segment: "",
                        delay: std::time::Duration::ZERO,
                        error: true,
                    }],
                    None,
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/a/SKILL.md"),
                        resource("skill://srv/b/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;
                assert!(
                    matches!(
                        reg.discovery_state("srv"),
                        Some(ServerDiscoveryState::Discovered { entries, .. })
                            if entries.is_empty()
                    ),
                    "全部 read 失败应回写空条目 Discovered"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        warns.iter().any(|m| m.contains("无可用条目")),
        "candidates 非空且全部 read 失败应发汇总 warn，实际: {warns:?}"
    );
}

/// cancel 提前退出 → clear_discovery_started 回退：首条响应后触发 cancel，
/// run_discovery 结束断言 discovery_state 为 None（Started 已清除）。
#[tokio::test]
async fn run_discovery_cancel_after_first_response_clears_started() {
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(4u32);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let first_done = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_secs(2),
            error: false,
        }],
        Some(Arc::clone(&first_done)),
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_discovery_handle(
        &running,
        vec![
            resource("skill://srv/zebra/SKILL.md"),
            resource("skill://srv/alpha/SKILL.md"),
        ],
    );
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    let discovery = tokio::spawn(run_discovery(
        reg.clone(),
        None,
        handle,
        token.clone(),
        cancel.clone(),
    ));

    // zebra 立即响应、alpha 延迟 2s → 首条响应后触发 cancel
    first_done.notified().await;
    cancel.cancel();
    discovery.await.unwrap();

    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 后应 clear_discovery_started 回退（discovery_state None）"
    );
}

/// cancel 提前退出不得误报汇总 warn：首条响应为 read 失败（entries 保持空），
/// 随后 cancel → 走 clear 路径，不经过 entries.is_empty() 的 warn 分支。
#[test]
fn run_discovery_cancel_before_warn_does_not_emit_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(5u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                let first_done = Arc::new(tokio::sync::Notify::new());
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![
                        // zebra：立即返回 error（entries 保持空）
                        RespondRule {
                            segment: "zebra",
                            delay: std::time::Duration::ZERO,
                            error: true,
                        },
                        // alpha：延迟 2s（cancel 在其完成前触发）
                        RespondRule {
                            segment: "alpha",
                            delay: std::time::Duration::from_secs(2),
                            error: false,
                        },
                    ],
                    Some(Arc::clone(&first_done)),
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/zebra/SKILL.md"),
                        resource("skill://srv/alpha/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                let discovery = tokio::spawn(run_discovery(
                    reg.clone(),
                    None,
                    handle,
                    token.clone(),
                    cancel.clone(),
                ));
                first_done.notified().await;
                cancel.cancel();
                discovery.await.unwrap();
                assert!(
                    reg.discovery_state("srv").is_none(),
                    "cancel 后应 clear_discovery_started 回退"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        !warns.iter().any(|m| m.contains("无可用条目")),
        "cancel 提前退出不得误报汇总 warn，实际: {warns:?}"
    );
}

// ─── run_discovery 级测试（SEP-2640 规范路径）─────────────────────────────

/// 规范模式测试 server：`skills/list` 返回固定条目（digest 由文本计算，
/// 可被 `digest_override` 篡改）；`resources/read` 返回条目文本；
/// `skills/get` 返回当前条目快照（可配置与 list 不同的内容模拟 stale 后
/// 更新，或返回 -32602 模拟 get 失败）。`request_log` 非 None 时按请求
/// 顺序记录 `"<method> <uri>"`。
#[derive(Clone)]
struct SpecSkill {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    /// list/read 用的内容（旧）
    text: &'static str,
    /// 非 None 时覆盖 skills/list 中的 digest（制造校验失败场景）
    digest_override: Option<String>,
    /// skills/get 返回的内容（None → 与 text 相同）
    get_text: Option<&'static str>,
    /// skills/get 对该 uri 返回 -32602（模拟 get 失败）
    get_error: bool,
    /// skills/get 返回错误 uri（与请求不一致，模拟 server 违规）
    get_wrong_uri: bool,
}

async fn spec_skill_server(
    io: tokio::io::DuplexStream,
    skills: Vec<SpecSkill>,
    first_done: Option<Arc<tokio::sync::Notify>>,
    request_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    // 已应答过 skills/get 的 uri 集合：模拟"stale → 更新"——get 后
    // resources/read 返回新内容（get_text），get 前返回旧内容（text）。
    let get_served: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Default::default();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            line.clear();
            continue;
        };
        line.clear();
        let writer = Arc::clone(&writer);
        let first_done = first_done.clone();
        let skills = skills.clone();
        let request_log = request_log.clone();
        let get_served = Arc::clone(&get_served);
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let method = parsed
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if let Some(log) = &request_log {
                log.lock().unwrap().push(format!("{method} {uri}"));
            }
            let response = match method.as_str() {
                "skills/list" => {
                    let entries: Vec<serde_json::Value> = skills
                        .iter()
                        .map(|s| {
                            let digest = match &s.digest_override {
                                Some(d) => d.clone(),
                                None => format!("sha256:{}", sha256_hex(s.text)),
                            };
                            serde_json::json!({
                                "uri": s.uri,
                                "frontmatter": { "name": s.name, "description": s.description },
                                "resources": [{ "uri": s.uri, "digest": digest }],
                            })
                        })
                        .collect();
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "skills": entries } })
                }
                "resources/read" => {
                    match skills.iter().find(|s| s.uri == uri) {
                        Some(s) => {
                            // stale→更新：get 已应答过的技能返回新内容
                            let served = get_served.lock().unwrap().contains(&uri);
                            let text = if served {
                                s.get_text.unwrap_or(s.text)
                            } else {
                                s.text
                            };
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
                                }
                            })
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown resource" }
                        }),
                    }
                }
                "skills/get" => {
                    // 当前条目快照（与 skills/list 条目同构）：digest 优先
                    // 沿用 list 的 digest（含 override——stale 快照一致性），
                    // 其次按 get 内容计算；get_error → -32602。应答后该技能
                    // 进入"已更新"态（后续读返回 get_text）。
                    match skills.iter().find(|s| s.uri == uri) {
                        Some(s) if s.get_error => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown skill" }
                        }),
                        Some(s) => {
                            get_served.lock().unwrap().insert(uri.clone());
                            let get_text = s.get_text.unwrap_or(s.text);
                            let digest = s
                                .digest_override
                                .clone()
                                .unwrap_or_else(|| format!("sha256:{}", sha256_hex(get_text)));
                            // uri 核对违规：返回与请求不一致的 uri（模拟
                            // server 违规，host 应拒绝恢复）。
                            let get_uri = if s.get_wrong_uri {
                                "skill://wrong/SKILL.md"
                            } else {
                                s.uri
                            };
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "skill": {
                                        "uri": get_uri,
                                        "frontmatter": { "name": s.name, "description": s.description },
                                        "resources": [{ "uri": s.uri, "digest": digest }],
                                    }
                                }
                            })
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown skill" }
                        }),
                    }
                }
                _ => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "method not found" }
                }),
            };
            let mut w = writer.lock().await;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            drop(w);
            if let Some(n) = &first_done {
                n.notify_one();
            }
        });
    }
}

/// 规范模式 handle：skills_capable=true，resources 为空也能发现
/// （skills/list 是独立原语，不依赖 resources 扫描）。
fn make_spec_handle(
    running: &rmcp::service::RunningService<RoleClient, ()>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: "srv".to_string(),
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: true,
        channel_capable: false,
    })
}

/// 规范模式端到端：skills/list 发现 → digest 校验通过的条目注册，
/// digest 不匹配的条目经 skills/get 恢复仍失败 → 被拒绝。
#[tokio::test]
async fn run_discovery_spec_mode_via_skills_list() {
    let ok_text = "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha\n";
    let bad_text = "---\nname: beta\ndescription: Beta skill\n---\n\n# Beta\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![
            SpecSkill {
                uri: "skill://alpha/SKILL.md",
                name: "alpha",
                description: "Alpha skill",
                text: ok_text,
                digest_override: None,
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
            // beta：digest 故意给错 → 内容校验失败 → skills/get 返回同一
            // stale 快照 → 恢复失败 → 拒绝
            SpecSkill {
                uri: "skill://beta/SKILL.md",
                name: "beta",
                description: "Beta skill",
                text: bad_text,
                digest_override: Some(format!("sha256:{}", "0".repeat(64))),
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
        ],
        None,
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(6u32);
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;

    let skills = reg.all_skills();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mcp__srv__alpha"],
        "digest 校验失败的条目被拒绝，仅注册通过校验的条目"
    );
    assert_eq!(
        skills[0].content.as_deref(),
        Some(ok_text),
        "content 存经校验的 SKILL.md 全文"
    );
    assert_eq!(
        skills[0].origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://alpha/SKILL.md".to_string(),
        })
    );
}

/// 规范模式端到端（skills/get 恢复）：digest 校验失败（stale）→
/// `skills/get` 拉取当前条目快照 → 按新内容重新校验注册；`skills/get`
/// 失败（-32602）→ 条目拒绝；frontmatter 比对失败不是 stale 信号 →
/// 不触发 skills/get。
#[tokio::test]
async fn run_discovery_spec_mode_recovers_via_skills_get() {
    let old_text = "---\nname: gamma\ndescription: Gamma skill\n---\n\n# Gamma v1\n";
    let new_text = "---\nname: gamma\ndescription: Gamma skill\n---\n\n# Gamma v2\n";
    let bad_text = "---\nname: delta\ndescription: Delta skill\n---\n\n# Delta\n";
    let fm_mismatch_text = "---\nname: epsilon\ndescription: Eps content\n---\n\n# Eps\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![
            // gamma：list 给新 digest + 旧内容（stale）→ get 给新快照
            // （新内容 + 同 digest）→ 恢复成功，按新内容注册
            SpecSkill {
                uri: "skill://gamma/SKILL.md",
                name: "gamma",
                description: "Gamma skill",
                text: old_text,
                digest_override: Some(format!("sha256:{}", sha256_hex(new_text))),
                get_text: Some(new_text),
                get_error: false,
                get_wrong_uri: false,
            },
            // delta：get 返回 -32602 → 恢复失败 → 拒绝
            SpecSkill {
                uri: "skill://delta/SKILL.md",
                name: "delta",
                description: "Delta skill",
                text: bad_text,
                digest_override: Some(format!("sha256:{}", "0".repeat(64))),
                get_text: None,
                get_error: true,
                get_wrong_uri: false,
            },
            // epsilon：digest 匹配但 frontmatter 比对失败（非 stale 信号）→
            // 不触发 skills/get，直接拒绝
            SpecSkill {
                uri: "skill://epsilon/SKILL.md",
                name: "epsilon",
                description: "Eps entry",
                text: fm_mismatch_text,
                digest_override: None,
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
        ],
        None,
        Some(Arc::clone(&request_log)),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(7u32);
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;

    let skills = reg.all_skills();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mcp__srv__gamma"],
        "仅经 skills/get 恢复成功的条目注册（delta get 失败拒绝、epsilon frontmatter 失败拒绝）"
    );
    assert_eq!(
        skills[0].content.as_deref(),
        Some(new_text),
        "content 为恢复后的新全文（按新条目重新读取校验）"
    );

    let log = request_log.lock().unwrap();
    assert!(
        log.iter().any(|e| e == "skills/get skill://gamma/SKILL.md"),
        "gamma digest 失败应触发 skills/get，实际: {log:?}"
    );
    assert!(
        log.iter().any(|e| e == "skills/get skill://delta/SKILL.md"),
        "delta digest 失败应触发 skills/get，实际: {log:?}"
    );
    assert!(
        !log.iter()
            .any(|e| e.starts_with("skills/get") && e.contains("epsilon")),
        "frontmatter 比对失败不是 stale 信号，不应触发 skills/get，实际: {log:?}"
    );
}

/// skills/get 响应 uri 核对（2026-08-15 review）：get 返回的条目 uri 与
/// 请求不一致（server 违规）→ 拒绝恢复（不按新 uri 重读），条目拒绝。
#[tokio::test]
async fn run_discovery_spec_mode_get_wrong_uri_rejects_recovery() {
    let zeta_text = "---\nname: zeta\ndescription: Zeta skill\n---\n\n# Zeta\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![SpecSkill {
            uri: "skill://zeta/SKILL.md",
            name: "zeta",
            description: "Zeta skill",
            text: zeta_text,
            // list 给错误 digest → 触发恢复
            digest_override: Some(format!("sha256:{}", "0".repeat(64))),
            get_text: None,
            get_error: false,
            // get 返回错误 uri（与请求不一致）→ 恢复被拒
            get_wrong_uri: true,
        }],
        None,
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(8u32);
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;

    assert!(
        reg.all_skills().is_empty(),
        "get 返回错误 uri → 拒绝恢复，条目不注册"
    );
}

// ─── mcp_route_entries（决策 1 命令面转换）─────────────────────────────────

/// 纯函数转换：SkillMetadata → mcp 域 RouteEntry（fullname `{server}:{skill}`
/// 小写、首段取 server 名末段、kind=McpSkill、provenance=Mcp+Discovered、
/// handler=McpSkillReleaser 放行跳板）。
#[test]
fn mcp_route_entries_converts_skills() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![
        SkillMetadata {
            name: "mcp__demo__AlphaSkill".to_string(),
            description: "Alpha skill".to_string(),
            path: PathBuf::new(),
            source: SkillSource::Mcp,
            plugin_name: None,
            origin: Some(SkillOrigin::Mcp {
                server: "demo".to_string(),
                uri: "skill://demo/alpha/SKILL.md".to_string(),
            }),
            content: None,
            resources: vec![],
        },
        // 同名消歧后形态：路径段作为 skill 名
        SkillMetadata {
            name: "mcp__demo__alpha/sub".to_string(),
            description: "Sub skill".to_string(),
            path: PathBuf::new(),
            source: SkillSource::Mcp,
            plugin_name: None,
            origin: None,
            content: None,
            resources: vec![],
        },
    ];
    let entries = mcp_route_entries(&registry, "demo", &skills);
    assert_eq!(entries.len(), 2, "全部转换");
    let e = &entries[0];
    assert_eq!(e.fullname, "demo:alphaskill", "fullname 小写归一");
    assert!(e.aliases.is_empty());
    assert_eq!(e.description, "Alpha skill");
    assert_eq!(e.kind, CommandEntryKind::McpSkill);
    assert_eq!(e.category, None);
    assert!(e.args_schema.is_none());
    assert_eq!(
        e.provenance.source,
        CommandSource::Mcp {
            server: "demo".to_string()
        }
    );
    assert_eq!(e.provenance.lifecycle, CommandLifecycle::Discovered);
    assert_eq!(
        entries[1].fullname, "demo:alpha/sub",
        "路径段形态保留（词法允许 /）"
    );
}

/// plugin 提供的 server key 形如 `plugin:{plugin}:{server}`（loader.rs:541），
/// 含冒号会突破词法 2 段上限 → 词法首段取末段；纯 server 名不变。
#[test]
fn mcp_route_entries_plugin_server_takes_last_segment() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "mcp__plugin:p1:demosrv__beta".to_string(),
        description: "Beta skill".to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: None,
        content: None,
        resources: vec![],
    }];
    let entries = mcp_route_entries(&registry, "plugin:p1:demosrv", &skills);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fullname, "demosrv:beta");
    assert_eq!(
        entries[0].provenance.source,
        CommandSource::Mcp {
            server: "demosrv".to_string()
        }
    );
}

/// P1-1：`mcp_source_key` 与 `mcp_route_entries` 的 fullname 首段派生
/// 同构（单一派生点 `mcp_namespace`）——断连注销前缀 `{末段}:` 才能
/// 命中条目 fullname（决策 1：键 = server 名末段，无 `mcp:` 域前缀）。
#[test]
fn mcp_source_key_matches_route_entry_namespace() {
    // 纯 server 名：键 = demo（不变）
    assert_eq!(mcp_source_key("demo"), "demo");
    // plugin server key：键 = demosrv（末段，非原名）
    assert_eq!(mcp_source_key("plugin:p1:demosrv"), "demosrv");
    // 键与条目 fullname 前缀一致（注销前缀 `{键}:` 命中 fullname）
    let registry = Arc::new(McpSkillRegistry::new());
    let entries = mcp_route_entries(
        &registry,
        "plugin:p1:demosrv",
        &[SkillMetadata {
            name: "mcp__plugin:p1:demosrv__beta".to_string(),
            description: "Beta skill".to_string(),
            ..SkillMetadata::default()
        }],
    );
    let key = mcp_source_key("plugin:p1:demosrv");
    for e in &entries {
        assert!(
            e.fullname.starts_with(&format!("{key}:")),
            "条目 fullname {} 必须以注销前缀 {key}: 开头",
            e.fullname
        );
    }
    // 大小写归一同源：大写 server 名同样派生小写键（与词法键一致）
    assert_eq!(mcp_source_key("Plugin:P1:DemoSrv"), "demosrv");
}

/// 审查 B1 防线 1：保留词法域 server 不进命令面——来源键派生标记 +
/// `finish_command_source` 跳过注册（元数据面不受影响，工具面仍可用）。
#[test]
fn mcp_source_key_reserved_domain_skipped() {
    for reserved in ["core", "ui", "plugin", "user", "mcp"] {
        assert!(
            mcp_namespace_reserved(reserved),
            "{reserved} 应判定为保留域"
        );
        // 纯 server 名末段命中保留域也判定（plugin:pa:core → core）。
        assert!(mcp_namespace_reserved(&format!("plugin:pa:{reserved}")));
    }
    assert!(!mcp_namespace_reserved("demo"));
    assert!(!mcp_namespace_reserved("plugin:pa:demosrv"));

    // finish_command_source：保留域 server 不产生任何命令面条目。
    let command_registry = Arc::new(CommandRegistry::new());
    let meta_registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "mcp__core__hello".to_string(),
        description: "Core-named skill".to_string(),
        ..SkillMetadata::default()
    }];
    let token_core: HandleToken = Arc::new(101u32);
    command_registry.mark_source_started("core", token_core.clone());
    finish_command_source(
        &Some(command_registry.clone()),
        &meta_registry,
        "core",
        token_core,
        &skills,
    );
    assert!(
        command_registry.resolve("core:hello").is_none(),
        "保留域 server 的 skill 不得注册为命令"
    );
    // 对照：非保留域正常注册。
    let skills2 = vec![SkillMetadata {
        name: "mcp__demo__hello".to_string(),
        description: "Demo skill".to_string(),
        ..SkillMetadata::default()
    }];
    let token_demo: HandleToken = Arc::new(102u32);
    command_registry.mark_source_started("demo", token_demo.clone());
    finish_command_source(
        &Some(command_registry.clone()),
        &meta_registry,
        "demo",
        token_demo,
        &skills2,
    );
    assert!(
        command_registry.resolve("demo:hello").is_some(),
        "非保留域 server 正常注册"
    );
}

/// 缺 `mcp__{server}__` 前缀的 skill 名 → 跳过（warn），不产出条目。
#[test]
fn mcp_route_entries_skips_unprefixed_name() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "other__name".to_string(),
        description: "No prefix".to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: None,
        content: None,
        resources: vec![],
    }];
    let entries = mcp_route_entries(&registry, "demo", &skills);
    assert!(entries.is_empty(), "缺前缀条目跳过");
}

/// run_discovery 命令面双写（决策 1）：规范模式发现完成后，命令面
/// 注册表收到 `srv:alpha` 条目（kind=McpSkill、provenance=Mcp+Discovered）。
#[tokio::test]
async fn run_discovery_writes_command_registry() {
    let ok_text = "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![SpecSkill {
            uri: "skill://alpha/SKILL.md",
            name: "alpha",
            description: "Alpha skill",
            text: ok_text,
            digest_override: None,
            get_text: None,
            get_error: false,
            get_wrong_uri: false,
        }],
        None,
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let token: HandleToken = Arc::new(9u32);
    reg.mark_discovery_started("srv", token.clone());
    cmd_reg.mark_source_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(
        reg.clone(),
        Some(cmd_reg.clone()),
        handle,
        token.clone(),
        cancel,
    )
    .await;

    let entries = cmd_reg.snapshot();
    assert_eq!(entries.len(), 1, "命令面注册 1 条");
    let e = &entries[0];
    assert_eq!(e.fullname, "srv:alpha");
    assert_eq!(e.kind, CommandEntryKind::McpSkill);
    assert_eq!(
        e.provenance.source,
        CommandSource::Mcp {
            server: "srv".to_string()
        }
    );
    assert_eq!(e.provenance.lifecycle, CommandLifecycle::Discovered);
    assert_eq!(e.description, "Alpha skill");
}

/// cancel 提前退出 → 命令面 Started 回退（与元数据面对齐，下轮可重试）。
#[tokio::test]
async fn run_discovery_cancel_clears_command_source_started() {
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let token: HandleToken = Arc::new(10u32);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let first_done = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_secs(2),
            error: false,
        }],
        Some(Arc::clone(&first_done)),
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_discovery_handle(
        &running,
        vec![
            resource("skill://srv/zebra/SKILL.md"),
            resource("skill://srv/alpha/SKILL.md"),
        ],
    );
    reg.mark_discovery_started("srv", token.clone());
    cmd_reg.mark_source_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    let discovery = tokio::spawn(run_discovery(
        reg.clone(),
        Some(cmd_reg.clone()),
        handle,
        token.clone(),
        cancel.clone(),
    ));

    // zebra 立即响应、alpha 延迟 2s → 首条响应后触发 cancel
    first_done.notified().await;
    cancel.cancel();
    discovery.await.unwrap();
    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 后元数据面 Started 已回退"
    );
    // 命令面 Started 回退后，来源无状态 → 下轮 before_agent 重新 to_discover；
    // 注册表无公开 sources 查询，用无条目 + 无 on_change 侧证（cancel 不触发）。
    assert!(cmd_reg.snapshot().is_empty(), "cancel 路径不注册条目");
}

// ─── McpSkillReleaser（决策 A2/D 放行跳板）─────────────────────────────────

/// 最小事件 sink（releaser execute 需要 CommandContext，对齐
/// plugin/loader_test.rs 先例）。
struct NoopEventSink;

#[async_trait::async_trait]
impl peri_acp_types::event::EventSink for NoopEventSink {
    async fn push_event(
        &self,
        _session_id: &str,
        _event: &peri_acp_types::event::ExecutorEvent,
        _context_window: u32,
    ) {
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 构造最小 CommandContext（supports_inject / raw_text 按场景置位）。
fn make_releaser_ctx(raw_text: &str, supports_inject: bool) -> CommandContext {
    let mut ctx = CommandContext::new(
        "s1".to_string(),
        vec![],
        "/tmp".to_string(),
        Arc::new(NoopEventSink),
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    ctx.raw_text = raw_text.to_string();
    ctx.supports_inject = supports_inject;
    ctx
}

/// 造 Discovered 条目（content 带全文；对齐 mcp_skills_test 的 complete）。
fn seed_discovered(reg: &Arc<McpSkillRegistry>, server: &str, skill: &str, content: &str) {
    let token: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: format!("mcp__{server}__{skill}"),
        description: format!("MCP skill {skill}"),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: format!("skill://{server}/{skill}/SKILL.md"),
        }),
        content: Some(content.to_string()),
        resources: vec![],
    };
    reg.mark_discovery_started(server, token.clone());
    reg.mark_discovery_completed(server, token, vec![meta]);
}

/// 交互式（supports_inject）：Inject(原文)——命令含 `/` 前缀与 args 整段
/// 放行进 agent 管线，由 SkillPreload 完成注入（决策 A2 核心语义）。
#[tokio::test]
async fn releaser_interactive_injects_raw_text() {
    let reg = Arc::new(McpSkillRegistry::new());
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("/demo:hello some args", true))
        .await;
    match outcome {
        CommandOutcome::Inject(text) => {
            assert_eq!(
                text, "/demo:hello some args",
                "原文整段放行（含 / 前缀与 args）"
            )
        }
        _other => panic!("交互式应 Inject 原文"),
    }
}

/// 交互式但原文缺失（理论不可达：拦截层恒透传）→ 回退 Done + Info，不吞
/// 命令不静默。
#[tokio::test]
async fn releaser_interactive_empty_raw_text_falls_back_done() {
    let reg = Arc::new(McpSkillRegistry::new());
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser.execute(make_releaser_ctx("", true)).await;
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty());
            let fb = result.feedback.expect("回退应有 Info 反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("原文缺失"),
                "反馈应说明原文缺失，实际: {}",
                fb.message
            );
        }
        _other => panic!("原文缺失应回退 Done"),
    }
}

/// RPC（supports_inject=false，决策 D）：直返 skill 全文 + 标注（与预载注入
/// 同源 annotate_mcp_content），feedback 说明语义差异。
#[tokio::test]
async fn releaser_rpc_returns_skill_content_with_annotation() {
    let reg = Arc::new(McpSkillRegistry::new());
    seed_discovered(&reg, "demo", "hello", "Body of hello.");
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("/demo:hello", false))
        .await;
    match outcome {
        CommandOutcome::Done(result) => {
            let last = result.messages.last().expect("RPC 命中应追加内容消息");
            let content = last.content();
            assert!(content.contains("Body of hello."), "应含 skill 全文");
            assert!(
                content.contains("This skill is served by MCP server \"demo\""),
                "应含来源标注，实际: {content}"
            );
            assert!(
                content.contains("SearchExtraTools"),
                "应含工具通路提醒，实际: {content}"
            );
            let fb = result.feedback.expect("RPC 命中应有反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("内容已返回"),
                "反馈应说明直返语义，实际: {}",
                fb.message
            );
        }
        _other => panic!("RPC 命中应直返 Done"),
    }
}

/// RPC 且 registry miss（server 未连接/无该 skill）→ Done + Info 提示，不
/// 吞命令不静默；messages 原样（不追加内容）。
#[tokio::test]
async fn releaser_rpc_miss_falls_back_done_info() {
    let reg = Arc::new(McpSkillRegistry::new());
    seed_discovered(&reg, "demo", "hello", "Body of hello.");
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("other:bye", false))
        .await;
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty(), "miss 不追加内容");
            let fb = result.feedback.expect("miss 应有反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("未发现"),
                "反馈应说明未发现，实际: {}",
                fb.message
            );
        }
        _other => panic!("RPC miss 应回退 Done"),
    }
}

#[tokio::test]
async fn cached_skill_discovery_avoids_list_and_skill_reads() {
    let text = "---\nname: cached\ndescription: Cached skill\n---\n\n# Cached\n";
    let request_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![SpecSkill {
            uri: "skill://cached/SKILL.md",
            name: "cached",
            description: "Cached skill",
            text,
            digest_override: None,
            get_text: None,
            get_error: false,
            get_wrong_uri: false,
        }],
        None,
        Some(Arc::clone(&request_log)),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    let origin = "test-skill-origin".to_string();

    let first = Arc::new(McpSkillRegistry::new());
    let first_token: HandleToken = Arc::new(41u32);
    first.mark_discovery_started("srv", first_token.clone());
    run_discovery_with_cache(
        first.clone(),
        None,
        make_spec_handle(&running),
        first_token,
        AgentCancellationToken::new(),
        Some((cache.clone(), origin.clone())),
    )
    .await;
    assert_eq!(first.all_skills().len(), 1);
    let first_requests = request_log.lock().unwrap().len();
    assert!(
        first_requests >= 2,
        "首次发现应请求 skills/list 与 resources/read"
    );

    let second = Arc::new(McpSkillRegistry::new());
    let second_token: HandleToken = Arc::new(42u32);
    second.mark_discovery_started("srv", second_token.clone());
    run_discovery_with_cache(
        second.clone(),
        None,
        make_spec_handle(&running),
        second_token,
        AgentCancellationToken::new(),
        Some((cache, origin)),
    )
    .await;

    assert_eq!(second.all_skills().len(), 1);
    assert_eq!(
        request_log.lock().unwrap().len(),
        first_requests,
        "skills/list 与通过校验的 SKILL.md 应均从持久化缓存读取"
    );
}
