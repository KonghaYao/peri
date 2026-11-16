//! 发现测试：legacy（select/parse/collect 纯函数 + run_discovery 级）与
//! SEP-2640 规范路径（skills/list 解析、digest 校验、frontmatter 比对、
//! 同名消歧、嵌套过滤、端到端 run_discovery）。

use super::PathBuf;
use super::Resource;
use super::RoleClient;
use super::Sha256;
use super::SkillListEntry;
use super::SkillListEntryDto;
use super::SkillListResponse;
use super::SkillMetadata;
use super::SkillOrigin;
use super::SkillResource;
use super::SkillSource;
use crate::mcp::client::{McpClientHandle, OAuthStatus};
use crate::mcp::skill_discovery::skills_list as skills_list_functions;
use crate::mcp::skill_discovery::skills_list::collect_entries;
use crate::mcp::skill_discovery::skills_list::SkillGetResponse;
use crate::mcp::ClientStatus;
use peri_acp_types::mcp_skills::ServerDiscoveryState;
use rmcp::model::ServerResult;
use sha2::Digest as _;
use std::fmt::Debug;
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
    let selected = super::select_skill_resources(&resources);
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
    assert!(super::select_skill_resources(&[]).is_empty());
    assert!(super::select_skill_resources(&[resource("https://x/SKILL.md")]).is_empty());
}

// ─── filter_nested_skills ──────────────────────────────────────────────────

#[test]
fn filter_nested_skills_keeps_top_level_only() {
    let resources = vec![
        resource("skill://root/SKILL.md"),
        resource("skill://root/sub/SKILL.md"), // 嵌套：root 是技能目录
        resource("skill://other/sub/SKILL.md"), // 非嵌套：无 skill://other/SKILL.md
    ];
    let kept = super::filter_nested_skills(resources);
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
    let meta = super::parse_mcp_skill_md(SAMPLE_MD, "demo", "skill://demo-skill/SKILL.md")
        .expect("应解析成功");
    assert_eq!(
        meta.name,
        super::mcp_skill_name("demo", "demo-skill"),
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
        super::parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 name → None"
    );
}

#[test]
fn parse_missing_description_returns_none() {
    let content = "---\nname: orphan\n---\n\n# Body\n";
    assert!(
        super::parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 description → None"
    );
}

#[test]
fn parse_invalid_yaml_returns_none() {
    assert!(
        super::parse_mcp_skill_md("not: [valid\nyaml", "srv", "skill://srv/SKILL.md").is_none()
    );
    assert!(
        super::parse_mcp_skill_md("# 无 frontmatter\n\n正文", "srv", "skill://srv/SKILL.md")
            .is_none()
    );
}

#[test]
fn parse_uri_final_segment_is_name_and_must_match_frontmatter() {
    // 带前缀：最终段 = sub（前缀 ns 是组织前缀，不入注册名）
    let content = "---\nname: sub\ndescription: d\n---\n";
    let meta = super::parse_mcp_skill_md(content, "srv", "skill://ns/sub/SKILL.md").unwrap();
    assert_eq!(meta.name, "mcp__srv__sub");

    // 非法字符（空格/点/β）sanitize 后比对一致 → 接受
    let content2 = "---\nname: v1.0-β\ndescription: d\n---\n";
    let meta2 =
        super::parse_mcp_skill_md(content2, "srv", "skill://my skill/v1.0-β/SKILL.md").unwrap();
    assert_eq!(meta2.name, "mcp__srv__v1_0-_");

    // 最终段与 frontmatter name 不一致 → 拒绝（身份必须可验证，防注入）
    let meta3 = super::parse_mcp_skill_md(
        "---\nname: other\ndescription: d\n---\n",
        "srv",
        "skill://a/SKILL.md",
    );
    assert!(meta3.is_none(), "uri 最终段 != frontmatter name → None");
}

#[test]
fn parse_uri_without_skill_prefix_or_suffix_returns_none() {
    assert!(
        super::parse_mcp_skill_md(SAMPLE_MD, "srv", "https://x/SKILL.md").is_none(),
        "非 skill:// 前缀 → None"
    );
    assert!(
        super::parse_mcp_skill_md(SAMPLE_MD, "srv", "skill://srv/other.md").is_none(),
        "非 /SKILL.md 后缀 → None"
    );
}

/// scheme 大小写不敏感（RFC 3986）：SKILL:// 前缀与 skill:// 同义。
#[test]
fn parse_uri_skill_scheme_case_insensitive() {
    let meta = super::parse_mcp_skill_md(SAMPLE_MD, "srv", "SKILL://demo-skill/SKILL.md").unwrap();
    assert_eq!(meta.name, "mcp__srv__demo-skill", "大写 scheme 应同样解析");
    let meta2 =
        super::parse_mcp_skill_md(SAMPLE_MD, "srv", "Skill://ns/demo-skill/SKILL.md").unwrap();
    assert_eq!(
        meta2.name, "mcp__srv__demo-skill",
        "混合大小写 scheme 应同样解析"
    );
}

#[test]
fn parse_description_trimmed() {
    // YAML 折叠标量尾部可能带 \n，与 loader.rs 一致做 trim
    let content = "---\nname: d\ndescription: >\n  hello\n  world\n---\n\n# Body\n";
    let meta = super::parse_mcp_skill_md(content, "srv", "skill://d/SKILL.md").unwrap();
    assert_eq!(meta.description, "hello world");
}

// ─── disambiguate_names ────────────────────────────────────────────────────

#[test]
fn disambiguate_names_on_collision_uses_path_segments() {
    let mk = |name: &str, uri: &str| SkillMetadata {
        name: name.to_string(),
        aliases: Vec::new(),
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
        frontmatter: None,
    };
    let entries = vec![
        mk("mcp__srv__refunds", "skill://acme/billing/refunds/SKILL.md"),
        mk("mcp__srv__refunds", "skill://acme/support/refunds/SKILL.md"),
        mk("mcp__srv__unique", "skill://unique/SKILL.md"),
    ];
    let out = super::disambiguate_names("srv", entries);
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
    assert!(super::verify_digest(SAMPLE_MD, &format!("sha256:{hex}")));
    assert!(!super::verify_digest(
        SAMPLE_MD,
        &format!("sha256:{}", "0".repeat(64))
    ));
    assert!(
        !super::verify_digest(SAMPLE_MD, &format!("SHA256:{hex}")),
        "前缀必须小写 sha256:"
    );
    assert!(!super::verify_digest(SAMPLE_MD, "md5:abc"));
    assert!(!super::verify_digest(SAMPLE_MD, &hex), "缺 sha256: 前缀");
    assert!(!super::verify_digest(SAMPLE_MD, "sha256:short"));
    // 大写 hex 通过（互操作宽容：server 生成大写 hex digest 是合法场景，
    // 校验前统一转小写比较）
    let upper = hex.to_uppercase();
    assert!(
        super::verify_digest(SAMPLE_MD, &format!("sha256:{upper}")),
        "大写 hex digest 应通过（互操作宽容）"
    );
    // 非 hex 字符拒绝（is_ascii_hexdigit 门闩）
    assert!(
        !super::verify_digest(SAMPLE_MD, &format!("sha256:{}", "g".repeat(64))),
        "g 非 hex 字符 → 拒绝"
    );
    assert!(
        !super::verify_digest(SAMPLE_MD, &format!("sha256:{}", "z".repeat(64))),
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
    let entry = super::entry_from_dto(dto).unwrap();
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
    let entry = super::entry_from_dto(omitted).unwrap();
    assert_eq!(entry.resources, None, "省略 resources → None（动态技能）");

    // 显式空数组 → Some(空)（present 但完整性违规，由 verify_and_build 拒绝）
    let empty: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": "A" },
        "resources": [],
    }))
    .unwrap();
    let entry = super::entry_from_dto(empty).unwrap();
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
    let entry = super::entry_from_dto(full).unwrap();
    assert_eq!(entry.resources.as_ref().unwrap().len(), 1);
}

#[test]
fn entry_from_dto_missing_name_reports_field_and_category() {
    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "description": "no name" },
    }))
    .unwrap();
    let defect = super::entry_from_dto(dto).unwrap_err();
    assert_eq!(defect.field, "name");
    assert_eq!(defect.category, "missing");
}

/// M8：非字符串 name/description 同样在 discovery 边界隔离（类别 wrong-type），
/// 诊断只带字段与类别，不回显字段正文。
#[test]
fn entry_from_dto_non_string_field_is_isolated() {
    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": 7, "description": { "nested": "value" } },
    }))
    .unwrap();
    let defect = super::entry_from_dto(dto).unwrap_err();
    assert_eq!(defect.field, "name");
    assert_eq!(defect.category, "wrong-type");

    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": 7 },
    }))
    .unwrap();
    let defect = super::entry_from_dto(dto).unwrap_err();
    assert_eq!(defect.field, "description");
    assert_eq!(defect.category, "wrong-type");
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
    let e0 = super::entry_from_dto(parsed.skills[0].clone()).unwrap();
    assert!(e0.resources.is_none(), "省略 resources → None");
    let e1 = super::entry_from_dto(parsed.skills[1].clone()).unwrap();
    assert_eq!(e1.resources.as_ref().unwrap().len(), 1);
}

/// [回归测试] 只带 `_meta` 的 result 不得被解析成工具结果。
///
/// rmcp 3.5.1 把 `_meta` 当 `CallToolResult` 的已知字段，导致 mcp-use 的
/// `skills/list` 响应在 untagged `ServerResult` 中先命中 `CallToolResult`；
/// 3.9.0 修复后仍须保证该响应不会被当成携带空技能目录的结果。
#[test]
fn skills_list_meta_only_result_is_not_a_tool_result() {
    let json = serde_json::json!({
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "mcp-use", "version": "2.12.4"}},
        "resultType": "complete"
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(!matches!(parsed, ServerResult::CallToolResult(_)));
    // 该响应不携带 skills 载荷：归一化必须失败，而不是冒充空目录。
    assert!(skills_list_functions::skill_result_payload(&parsed).is_none());
}

/// [回归测试] 完整 skills + 带 `size` 的 resources 命中 3.9.0 的
/// `ListSkillsResult`，归一化后仍按 `SkillListResponse` 解析（`size` 被忽略）。
#[test]
fn skills_list_typed_result_with_sizes_normalizes() {
    let json = serde_json::json!({
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "mcp-use"}},
        "skills": [{
            "uri": "skill://a/SKILL.md",
            "frontmatter": {"name": "a", "description": "A"},
            "resources": [{"uri": "skill://a/SKILL.md", "digest": "sha256:abcd", "size": 42}]
        }]
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ServerResult::ListSkillsResult(_)));
    let payload = skills_list_functions::skill_result_payload(&parsed).expect("typed payload");
    let response: SkillListResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(response.skills.len(), 1);
    let entry = super::entry_from_dto(response.skills[0].clone()).unwrap();
    assert_eq!(
        entry.resources.unwrap()[0].digest,
        "sha256:abcd",
        "size 不在领域模型内，digest 必须保留"
    );
}

/// [回归测试] 动态技能的 `resources` 在类型化结果中是字符串 `"dynamic"`：
/// 归一化后按缺省 resources 解析，同页静态技能不得因整页解析失败而丢失。
#[test]
fn skills_list_typed_result_with_dynamic_skill_keeps_static_siblings() {
    let json = serde_json::json!({
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "mcp-use"}},
        "skills": [
            {
                "uri": "skill://static/SKILL.md",
                "frontmatter": {"name": "static", "description": "S"},
                "resources": [{"uri": "skill://static/SKILL.md", "digest": "sha256:abcd", "size": 42}]
            },
            {
                "uri": "skill://dynamic/SKILL.md",
                "frontmatter": {"name": "dynamic", "description": "D"},
                "resources": "dynamic"
            }
        ]
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ServerResult::ListSkillsResult(_)));
    let payload = skills_list_functions::skill_result_payload(&parsed).expect("typed payload");
    let response: SkillListResponse =
        serde_json::from_value(payload).expect("含动态技能的整页仍可解析");
    assert_eq!(response.skills.len(), 2);
    let static_entry = super::entry_from_dto(response.skills[0].clone()).unwrap();
    assert_eq!(
        static_entry.resources.expect("静态技能 manifest")[0].digest,
        "sha256:abcd"
    );
    let dynamic_entry = super::entry_from_dto(response.skills[1].clone()).unwrap();
    assert!(
        dynamic_entry.resources.is_none(),
        "动态技能无 digest：归一化为缺省 resources（保守激活）"
    );
}

/// [回归测试] 省略 `resources`（mcp-use 实际形状）落到 `CustomResult`，
/// 归一化后仍可解析；与类型化分支共用同一领域响应。
#[test]
fn skills_list_custom_result_without_resources_normalizes() {
    let json = serde_json::json!({
        "skills": [{"uri": "skill://a/SKILL.md", "frontmatter": {"name": "a", "description": "A"}}]
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ServerResult::CustomResult(_)));
    let payload = skills_list_functions::skill_result_payload(&parsed).expect("custom payload");
    let response: SkillListResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(response.skills.len(), 1);
    let entry = super::entry_from_dto(response.skills[0].clone()).unwrap();
    assert!(entry.resources.is_none());
}

/// [回归测试] `skills/get` 的完整条目命中 `GetSkillResult`，归一化后按
/// `SkillGetResponse` 解析（与 list 条目同构）。
#[test]
fn skills_get_typed_result_normalizes() {
    let json = serde_json::json!({
        "skill": {
            "uri": "skill://a/SKILL.md",
            "frontmatter": {"name": "a", "description": "A"},
            "resources": [{"uri": "skill://a/SKILL.md", "digest": "sha256:abcd", "size": 42}]
        }
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ServerResult::GetSkillResult(_)));
    let payload = skills_list_functions::skill_result_payload(&parsed).expect("typed payload");
    let response: SkillGetResponse = serde_json::from_value(payload).unwrap();
    let entry = super::entry_from_dto(response.skill).unwrap();
    assert_eq!(entry.uri, "skill://a/SKILL.md");
}

/// [回归测试] `skills/get` 的动态技能同样把字符串 `"dynamic"` 归一化为缺省
/// resources；DTO 本身仍拒绝任意字符串。
#[test]
fn skills_get_typed_result_with_dynamic_resources_normalizes() {
    let json = serde_json::json!({
        "skill": {
            "uri": "skill://a/SKILL.md",
            "frontmatter": {"name": "a", "description": "A"},
            "resources": "dynamic"
        }
    });
    let parsed: ServerResult = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ServerResult::GetSkillResult(_)));
    let payload = skills_list_functions::skill_result_payload(&parsed).expect("typed payload");
    let response: SkillGetResponse = serde_json::from_value(payload).unwrap();
    let entry = super::entry_from_dto(response.skill).unwrap();
    assert!(
        entry.resources.is_none(),
        "动态技能按缺省 resources 保守激活"
    );
}

// ─── entry_to_metadata（W2：发现期只做结构校验，不读正文）─────────────────

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
fn entry_to_metadata_builds_metadata_without_content() {
    let meta = super::entry_to_metadata("srv", &spec_entry(None)).expect("结构合法应构建");
    assert_eq!(meta.name, "mcp__srv__a");
    assert_eq!(meta.description, "A skill");
    assert_eq!(
        meta.origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://a/SKILL.md".to_string(),
        })
    );
    // W2：发现只发布 metadata——正文与 frontmatter 快照留给 activation。
    assert!(meta.content.is_none(), "发现期不得携带正文");
    assert!(
        meta.frontmatter.is_some(),
        "必须携带 frontmatter 快照供激活比对"
    );
    assert_eq!(meta.resources.len(), 1, "resources 清单保留供内容绑定");
}

#[test]
fn entry_to_metadata_rejects_uri_name_mismatch() {
    let mut entry = spec_entry(None);
    entry.uri = "skill://other/SKILL.md".to_string();
    entry.resources = Some(vec![SkillResource {
        uri: "skill://other/SKILL.md".to_string(),
        digest: format!("sha256:{}", "0".repeat(64)),
    }]);
    assert!(
        super::entry_to_metadata("srv", &entry).is_none(),
        "URI 最终段与 frontmatter name 不一致（身份失败）→ 拒绝"
    );
}

#[test]
fn entry_to_metadata_without_resources_is_accepted_but_unbound() {
    let mut entry = spec_entry(None);
    entry.resources = None;
    let meta =
        super::entry_to_metadata("srv", &entry).expect("省略 resources（动态技能）条目仍可发现");
    assert!(meta.resources.is_empty(), "无内容绑定：激活按保守策略拒绝");
}

#[test]
fn entry_to_metadata_empty_resources_rejected() {
    let mut entry = spec_entry(None);
    entry.resources = Some(Vec::new());
    assert!(
        super::entry_to_metadata("srv", &entry).is_none(),
        "显式空数组（present 但不完整）→ 完整性违规，拒绝"
    );
}

#[test]
fn entry_to_metadata_resources_missing_self_rejected() {
    let mut entry = spec_entry(None);
    entry.resources = Some(vec![SkillResource {
        uri: "skill://a/other.md".to_string(),
        digest: format!("sha256:{}", "0".repeat(64)),
    }]);
    assert!(
        super::entry_to_metadata("srv", &entry).is_none(),
        "resources 未含 SKILL.md 自身条目 → 完整性违规，拒绝"
    );
}

#[test]
fn entries_to_metadata_sorts_and_reports_all_rejected() {
    let entries = vec![
        SkillListEntry {
            uri: "skill://z/SKILL.md".to_string(),
            frontmatter: serde_json::json!({ "name": "z", "description": "Z" })
                .as_object()
                .unwrap()
                .clone(),
            resources: Some(vec![SkillResource {
                uri: "skill://z/SKILL.md".to_string(),
                digest: format!("sha256:{}", "1".repeat(64)),
            }]),
        },
        spec_entry(None),
    ];
    let (all_rejected, metas) = super::entries_to_metadata("srv", entries);
    assert!(!all_rejected);
    assert_eq!(
        metas.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        vec!["mcp__srv__a", "mcp__srv__z"],
        "输出按注册名排序（完成序无关）"
    );

    let mut broken = spec_entry(None);
    broken.uri = "skill://mismatch/SKILL.md".to_string();
    let (all_rejected, metas) = super::entries_to_metadata("srv", vec![broken]);
    assert!(
        all_rejected && metas.is_empty(),
        "候选非空但全部结构非法 → 上报汇总 warn 信号"
    );
}

/// frontmatter_maps_equal 纯函数单测：键集合/值类型归一/嵌套递归。
#[test]
fn frontmatter_maps_equal_units() {
    use serde_json::{json, Map, Value};
    let map = |v: Value| v.as_object().unwrap().clone();
    // 全等（含嵌套）
    let a = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] }, "license": "MIT" }));
    let b = map(json!({ "license": "MIT", "meta": { "tags": ["t1", 2] }, "name": "a" }));
    assert!(super::frontmatter_maps_equal(&a, &b), "键序无关的全等");
    // 附加字段差异 → 不相等
    let c = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] }, "license": "Apache" }));
    assert!(
        !super::frontmatter_maps_equal(&a, &c),
        "license 差异 → 不等"
    );
    // 键缺失 → 不相等
    let d = map(json!({ "name": "a", "meta": { "tags": ["t1", 2] } }));
    assert!(
        !super::frontmatter_maps_equal(&a, &d),
        "缺 license 键 → 不等"
    );
    // 数字 ↔ 字符串跨类型严格化（2026-08-15 定案）：42 ≠ "42"
    assert!(!super::frontmatter_values_equal(
        &json!("42"),
        &json!(42u64)
    ));
    assert!(!super::frontmatter_values_equal(
        &json!(42u64),
        &json!("42")
    ));
    assert!(!super::frontmatter_values_equal(
        &json!(1.0f64),
        &json!("1")
    ));
    assert!(!super::frontmatter_values_equal(
        &json!("42"),
        &json!(43u64)
    ));
    assert!(!super::frontmatter_values_equal(
        &json!("abc"),
        &json!(42u64)
    ));
    // Number vs Number 保留 serde_json 混合 f64 比较（1 == 1.0 成立）
    assert!(super::frontmatter_values_equal(
        &json!(1u64),
        &json!(1.0f64)
    ));
    assert!(super::frontmatter_values_equal(
        &json!(42u64),
        &json!(42u64)
    ));
    // String vs String 尾随空白归一（仅 trim_end，不做 trim 全量）
    assert!(super::frontmatter_values_equal(
        &json!("hello\n"),
        &json!("hello")
    ));
    assert!(!super::frontmatter_values_equal(
        &json!(" hello"),
        &json!("hello")
    ));
    // 嵌套数组内跨类型 → 不等（与顶层同规则）
    let e = map(json!({ "n": [1, "2"] }));
    let f = map(json!({ "n": ["1", 2] }));
    assert!(
        !super::frontmatter_maps_equal(&e, &f),
        "嵌套数组跨类型 → 不等"
    );
    // 跨类型（bool vs 字符串）→ 不等
    assert!(!super::frontmatter_values_equal(
        &json!(true),
        &json!("true")
    ));
    // 空 map 全等
    let empty: Map<String, Value> = Map::new();
    assert!(super::frontmatter_maps_equal(&empty, &empty));
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
    assert!(super::frontmatter_values_equal(
        &json!(9007199254740992f64),
        &json!(9007199254740992u64)
    ));
    // 域内精确比较：相邻大整数不被 f64 舍入糊掉（旧代码：
    // 9007199254740993u64 as f64 舍入为 9007199254740992.0 → 误判相等）
    assert!(!super::frontmatter_values_equal(
        &json!(9007199254740992f64),
        &json!(9007199254740993u64)
    ));
    // 域外保守拒绝：2^53+2 与同名整数 f64 表示相同，但超出 f64 精确整数域
    // → 判不等（旧代码误判相等）
    assert!(!super::frontmatter_values_equal(
        &json!(9007199254740994f64),
        &json!(9007199254740994u64)
    ));
    // 负域：-2^53 边界精确相等；-2^53-2 域外保守拒绝
    assert!(super::frontmatter_values_equal(
        &json!(-9007199254740992f64),
        &json!(-9007199254740992i64)
    ));
    assert!(!super::frontmatter_values_equal(
        &json!(-9007199254740994f64),
        &json!(-9007199254740994i64)
    ));
    // 域内小整数回归：1 == 1.0 仍成立；非整数 Float 不与整数相等
    assert!(super::frontmatter_values_equal(
        &json!(1u64),
        &json!(1.0f64)
    ));
    assert!(!super::frontmatter_values_equal(
        &json!(1u64),
        &json!(1.5f64)
    ));
}

include!("skill_discovery_test/skill_server_fixtures.rs");

#[path = "skill_discovery_legacy_test.rs"]
mod legacy_discovery_tests;

#[path = "skill_discovery/cache_fixture_test.rs"]
mod cache_fixture;

#[path = "skill_discovery_spec_test.rs"]
mod spec_discovery_tests;

#[path = "skill_discovery_commands_test.rs"]
mod command_projection_tests;

#[path = "skill_discovery_releaser_test.rs"]
mod releaser_tests;

#[path = "skill_discovery/entry_validation_test.rs"]
mod entry_validation_tests;

#[path = "skill_discovery/catalog_state_test.rs"]
mod catalog_state_tests;
