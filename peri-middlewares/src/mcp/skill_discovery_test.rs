//! 发现测试：legacy（select/parse/collect 纯函数 + run_discovery 级）与
//! SEP-2640 规范路径（skills/list 解析、digest 校验、frontmatter 比对、
//! 同名消歧、嵌套过滤、端到端 run_discovery）。

use super::*;
use crate::mcp::client::{McpClientHandle, OAuthStatus};
use crate::mcp::skill_discovery::skills_list::collect_entries;
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
fn entry_from_dto_missing_name_reports_field_and_category() {
    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "description": "no name" },
    }))
    .unwrap();
    let defect = entry_from_dto(dto).unwrap_err();
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
    let defect = entry_from_dto(dto).unwrap_err();
    assert_eq!(defect.field, "name");
    assert_eq!(defect.category, "wrong-type");

    let dto: SkillListEntryDto = serde_json::from_value(serde_json::json!({
        "uri": "skill://a/SKILL.md",
        "frontmatter": { "name": "a", "description": 7 },
    }))
    .unwrap();
    let defect = entry_from_dto(dto).unwrap_err();
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
    let e0 = entry_from_dto(parsed.skills[0].clone()).unwrap();
    assert!(e0.resources.is_none(), "省略 resources → None");
    let e1 = entry_from_dto(parsed.skills[1].clone()).unwrap();
    assert_eq!(e1.resources.as_ref().unwrap().len(), 1);
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
    let meta = entry_to_metadata("srv", &spec_entry(None)).expect("结构合法应构建");
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
        entry_to_metadata("srv", &entry).is_none(),
        "URI 最终段与 frontmatter name 不一致（身份失败）→ 拒绝"
    );
}

#[test]
fn entry_to_metadata_without_resources_is_accepted_but_unbound() {
    let mut entry = spec_entry(None);
    entry.resources = None;
    let meta = entry_to_metadata("srv", &entry).expect("省略 resources（动态技能）条目仍可发现");
    assert!(meta.resources.is_empty(), "无内容绑定：激活按保守策略拒绝");
}

#[test]
fn entry_to_metadata_empty_resources_rejected() {
    let mut entry = spec_entry(None);
    entry.resources = Some(Vec::new());
    assert!(
        entry_to_metadata("srv", &entry).is_none(),
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
        entry_to_metadata("srv", &entry).is_none(),
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
    let (all_rejected, metas) = entries_to_metadata("srv", entries);
    assert!(!all_rejected);
    assert_eq!(
        metas.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        vec!["mcp__srv__a", "mcp__srv__z"],
        "输出按注册名排序（完成序无关）"
    );

    let mut broken = spec_entry(None);
    broken.uri = "skill://mismatch/SKILL.md".to_string();
    let (all_rejected, metas) = entries_to_metadata("srv", vec![broken]);
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

async fn private_zero_ttl_skill_server(
    io: tokio::io::DuplexStream,
    request_count: Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let parsed = serde_json::from_str::<serde_json::Value>(line.trim_end()).ok();
        line.clear();
        let Some(parsed) = parsed else { continue };
        let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let uri = parsed["params"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = "---\nname: cached\ndescription: Cached skill\n---\n\n# Cached\n";
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }],
                "cacheScope": "private",
                "ttlMs": 0,
            }
        });
        let mut writer = writer.lock().await;
        writer
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }
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
        version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
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
    cacheable: bool,
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
                    let mut result = serde_json::json!({ "skills": entries });
                    if cacheable {
                        result["cacheScope"] = serde_json::json!("public");
                        result["ttlMs"] = serde_json::json!(60_000);
                    }
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
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
                            let mut result = serde_json::json!({
                                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
                            });
                            if cacheable {
                                result["cacheScope"] = serde_json::json!("public");
                                result["ttlMs"] = serde_json::json!(60_000);
                            }
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result
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
        version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: true,
    })
}

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
