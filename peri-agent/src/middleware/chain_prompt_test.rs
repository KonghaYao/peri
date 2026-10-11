use super::*;
use crate::middleware::r#trait::{Middleware, NoopMiddleware};
use crate::middleware::{
    project_enabled_sections,
    prompt_sections::{PromptSection, PromptSectionZone},
};
use async_trait::async_trait;

/// 声明段落的中间件（collect_prompt_sections 测试；默认无段落，契约 4）
struct SectionProvider {
    name: String,
    sections: Vec<PromptSection>,
}

impl SectionProvider {
    fn new(name: &str, sections: Vec<PromptSection>) -> Self {
        Self {
            name: name.to_string(),
            sections,
        }
    }
}

#[async_trait]
impl Middleware for SectionProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn prompt_sections(&self) -> Vec<PromptSection> {
        self.sections.clone()
    }
}

#[test]
fn test_collect_prompt_sections_empty_chain() {
    let chain = MiddlewareChain::new();
    assert!(
        chain.collect_prompt_sections().is_empty(),
        "空链收集为空（契约 4：未提供段落不 fail）"
    );
}

#[test]
fn test_collect_prompt_sections_gathers_provided_sections() {
    let mut chain = MiddlewareChain::new();
    // 无段落的中间件（默认实现）+ 声明段落的中间件混合
    chain.add(Box::new(NoopMiddleware::new("NoSections")));
    chain.add(Box::new(SectionProvider::new(
        "SectionHolder",
        vec![
            PromptSection::builtin("10_hitl", PromptSectionZone::Uncached, 3, "hitl"),
            PromptSection::dynamic("zz_dyn", PromptSectionZone::Uncached, 8, "dyn".to_string()),
        ],
    )));
    chain.add(Box::new(SectionProvider::new("EmptyHolder", vec![])));

    let collected = chain.collect_prompt_sections();
    let ids: Vec<&str> = collected.iter().map(|s| s.id).collect();
    assert_eq!(ids, vec!["10_hitl", "zz_dyn"], "仅收集声明段落的中间件");
    // 内容与位置属性透传
    let hitl = collected
        .iter()
        .find(|s| s.id == "10_hitl")
        .expect("10_hitl 在收集结果中");
    assert_eq!(hitl.zone, PromptSectionZone::Uncached);
    assert_eq!(hitl.order, 3);
    assert_eq!(hitl.content.as_str(), "hitl");
    let dyn_section = collected.iter().find(|s| s.id == "zz_dyn").unwrap();
    assert_eq!(dyn_section.content.as_str(), "dyn", "动态内容透传");
}

/// 契约 3 投影：段落 gate = 持有 middleware 是否在链上（映射表驱动）。
#[test]
fn test_project_enabled_sections_from_chain_names() {
    use std::collections::HashSet;

    // 空集合 → 无段落开启
    assert!(project_enabled_sections(&HashSet::new()).is_empty());

    let names: HashSet<&str> = [
        "SubAgentMiddleware",
        "SkillsMiddleware",
        "UnrelatedMiddleware",
    ]
    .into_iter()
    .collect();
    let enabled = project_enabled_sections(&names);
    assert!(
        enabled.contains("11_subagent"),
        "SubAgentMiddleware 在链上 → 11_subagent 开启"
    );
    assert!(
        enabled.contains("13_skills"),
        "SkillsMiddleware 在链上 → 13_skills 开启"
    );
    assert!(
        !enabled.contains("10_hitl"),
        "PermissionMiddleware 不在链上 → 10_hitl 关闭（2026-08-15 拆分：10_hitl 持有者）"
    );
    assert!(
        !enabled.contains("16_workflow"),
        "16_workflow 已整段删除（C2，ultracode skill 覆盖），投影恒不含"
    );

    // 与链收集的一致性：链上持有者提供的段落 = 投影开启的段落
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(SectionProvider::new(
        "SkillsMiddleware",
        vec![PromptSection::builtin(
            "13_skills",
            PromptSectionZone::Uncached,
            5,
            "skills",
        )],
    )));
    let collected_ids: HashSet<&str> = chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    let projected = project_enabled_sections(&chain.names().into_iter().collect::<HashSet<&str>>());
    assert_eq!(
        collected_ids, projected,
        "收集到的段落 = 映射表投影（同一判定，两条路径一致）"
    );
}

/// 贡献中间件：按名返回固定 prompt_contribution。
struct ContributionStub {
    name: &'static str,
    contribution: Option<String>,
}

#[async_trait]
impl Middleware for ContributionStub {
    fn name(&self) -> &str {
        self.name
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.contribution.clone()
    }
}

/// M1：非空贡献统一以空行连接（调用方不再补分隔符）；空贡献被跳过。
#[test]
fn contributions_are_joined_with_blank_line() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ContributionStub {
        name: "First",
        contribution: Some("FIRST-BODY".to_string()),
    }));
    chain.add(Box::new(ContributionStub {
        name: "Empty",
        contribution: Some(String::new()),
    }));
    chain.add(Box::new(ContributionStub {
        name: "Second",
        contribution: Some("SECOND-BODY".to_string()),
    }));
    chain.add(Box::new(ContributionStub {
        name: "None",
        contribution: None,
    }));

    let collected = chain.collect_prompt_contributions().expect("合法贡献");
    assert_eq!(collected, "FIRST-BODY\n\nSECOND-BODY");
}

/// M1：单个贡献原样返回（无前导/尾随分隔符）。
#[test]
fn single_contribution_has_no_added_separator() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ContributionStub {
        name: "Only",
        contribution: Some("ONLY-BODY".to_string()),
    }));
    assert_eq!(
        chain.collect_prompt_contributions().expect("合法贡献"),
        "ONLY-BODY"
    );
}

/// M1：含 reserved boundary token 的贡献返回带来源的错误——不静默剥离内容，
/// 也不继续构造歧义请求。
#[test]
fn contribution_with_reserved_boundary_token_fails_with_source() {
    let token = peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ContributionStub {
        name: "Poisoned",
        contribution: Some(format!("BEFORE{token}AFTER")),
    }));

    let error = chain
        .collect_prompt_contributions()
        .expect_err("非法贡献必须显式失败");
    assert_eq!(error.middleware(), "Poisoned");
    assert!(error.to_string().contains("Poisoned"));
}
