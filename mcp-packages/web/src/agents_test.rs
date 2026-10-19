//! web 实例 builtin Agent 资源面的 crate 内证据。
//!
//! 覆盖：定义与静态表一致（id / frontmatter / digest）、资源列表的 URI 与
//! `_meta` 投影、read 的逐字正文与范围判定（非本实例 URI 不误读）。

use super::*;
use peri_acp_types::workspace_resources::{digest_bytes, META_KEY_DIGEST, META_KEY_SCOPE};

const WEB_RESEARCHER_URI: &str = "agent://builtin/web-researcher/agent.md";

#[test]
fn web_researcher_is_the_only_shipped_agent_and_matches_its_frontmatter() {
    assert_eq!(BUILTIN_AGENTS.len(), 1, "web 实例只发布 web-researcher");
    let agent = &BUILTIN_AGENTS[0];
    assert_eq!(agent.id, "web-researcher");

    let frontmatter = frontmatter_json(agent.content).expect("frontmatter 必须可解析");
    assert_eq!(
        frontmatter.get("name").and_then(|v| v.as_str()),
        Some(agent.id)
    );
    assert_eq!(
        frontmatter.get("model").and_then(|v| v.as_str()),
        Some("haiku")
    );
    assert!(
        frontmatter
            .get("disallowedTools")
            .and_then(|v| v.as_array())
            .is_some_and(|tools| tools.iter().any(|tool| tool == "Agent")),
        "web-researcher 必须禁止递归派发 Agent"
    );
}

#[test]
fn list_projects_the_builtin_uri_and_metadata() {
    let resources = list_resources();
    assert_eq!(resources.len(), 1);
    let resource = &resources[0];
    assert_eq!(resource.uri, WEB_RESEARCHER_URI);
    assert_eq!(resource.name, "web-researcher");
    assert_eq!(resource.mime_type.as_deref(), Some(MIME_MARKDOWN));
    assert!(
        resource
            .description
            .as_deref()
            .is_some_and(|text| !text.is_empty()),
        "资源描述取 frontmatter description"
    );
    // name 与 id 相同 ⇒ 不额外写 title。
    assert!(resource.title.is_none());

    let meta = resource.meta.as_ref().expect("资源必须带 _meta");
    assert_eq!(
        meta.0.get(META_KEY_SCOPE).and_then(|v| v.as_str()),
        Some("builtin")
    );
    assert_eq!(
        meta.0.get(META_KEY_DIGEST).and_then(|v| v.as_str()),
        Some(digest_bytes(BUILTIN_AGENTS[0].content.as_bytes()).as_str())
    );
    let frontmatter = meta
        .0
        .get(META_KEY_FRONTMATTER)
        .and_then(|value| value.as_object())
        .expect("目录渲染依赖 frontmatter 投影");
    assert_eq!(
        frontmatter.get("name").and_then(|v| v.as_str()),
        Some("web-researcher")
    );
}

#[test]
fn read_returns_the_definition_verbatim_with_metadata() {
    let contents = read(WEB_RESEARCHER_URI).expect("本实例定义可读");
    match contents {
        ResourceContents::TextResourceContents {
            text,
            mime_type,
            meta,
            ..
        } => {
            assert_eq!(text, BUILTIN_AGENTS[0].content, "正文逐字返回");
            assert_eq!(mime_type.as_deref(), Some(MIME_MARKDOWN));
            let meta = meta.expect("读取面同样投影 _meta");
            assert_eq!(
                meta.0.get(META_KEY_SCOPE).and_then(|v| v.as_str()),
                Some("builtin")
            );
            assert_eq!(
                meta.0.get(META_KEY_DIGEST).and_then(|v| v.as_str()),
                Some(digest_bytes(text.as_bytes()).as_str())
            );
        }
        other => panic!("agent 定义必须是文本内容：{other:?}"),
    }
}

#[test]
fn read_rejects_non_agent_and_foreign_uris_without_leaking_the_definition() {
    assert_eq!(
        read("https://example.test/x").unwrap_err(),
        AgentResourceError::InvalidUri
    );
    assert_eq!(
        read("agent://builtin/not-shipped/agent.md").unwrap_err(),
        AgentResourceError::NotFound
    );
    // 其他 scope 形状合法，但不归本实例提供。
    for uri in [
        "agent://project/web-researcher/agent.md",
        "agent://plugin/plug/web-researcher/agent.md",
        "agent://user/web-researcher/agent.md",
    ] {
        assert_eq!(
            read(uri).unwrap_err(),
            AgentResourceError::NotFound,
            "{uri}"
        );
    }
}
