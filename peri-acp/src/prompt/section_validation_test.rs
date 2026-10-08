//! 段落覆盖准入校验（L3）与 Cached / 装配冲突（M11）测试。

use super::section_validation::*;
use super::*;
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_middlewares::host_ports::AgentCatalogProvider;

fn section(
    id: &'static str,
    zone: PromptSectionZone,
    order: u16,
    content: &'static str,
) -> PromptSection {
    PromptSection::builtin(id, zone, order, content)
}

fn state_with_override(id: &str, content: &str) -> MetaHarnessState {
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert(id.to_string(), Arc::from(content));
    state
}

// ─── L3：单段校验 ────────────────────────────────────────────────────────

#[test]
fn empty_override_is_rejected() {
    for text in ["", "   ", "\n\t "] {
        assert_eq!(
            validate_section_override(PromptSectionZone::Uncached, text),
            Err(OverrideRejection::Empty),
            "空白覆盖必须显式拒绝并保留内置段"
        );
    }
}

#[test]
fn oversized_override_is_rejected_and_limit_boundary_accepted() {
    let at_limit = "a".repeat(MAX_SECTION_OVERRIDE_BYTES);
    assert!(validate_section_override(PromptSectionZone::Uncached, &at_limit).is_ok());

    let over = "a".repeat(MAX_SECTION_OVERRIDE_BYTES + 1);
    assert_eq!(
        validate_section_override(PromptSectionZone::Uncached, &over),
        Err(OverrideRejection::TooLarge {
            bytes: MAX_SECTION_OVERRIDE_BYTES + 1,
            limit: MAX_SECTION_OVERRIDE_BYTES,
        })
    );
}

#[test]
fn reserved_boundary_token_is_rejected() {
    let text = format!("前置 {SYSTEM_PROMPT_DYNAMIC_BOUNDARY} 后置");
    assert_eq!(
        validate_section_override(PromptSectionZone::Uncached, &text),
        Err(OverrideRejection::ReservedBoundaryToken)
    );
}

#[test]
fn unknown_placeholder_is_rejected_known_one_accepted_in_uncached() {
    assert_eq!(
        validate_section_override(PromptSectionZone::Uncached, "{{unknown_name}}"),
        Err(OverrideRejection::UnknownPlaceholder(
            "unknown_name".to_string()
        ))
    );
    assert_eq!(
        validate_section_override(PromptSectionZone::Uncached, "{{unterminated"),
        Err(OverrideRejection::UnknownPlaceholder(
            "<unterminated>".to_string()
        ))
    );
    for known in KNOWN_PLACEHOLDERS {
        let text = format!("value: {{{{{known}}}}}");
        assert!(
            validate_section_override(PromptSectionZone::Uncached, &text).is_ok(),
            "已知占位符 {known} 应被接受（Uncached）"
        );
    }
}

#[test]
fn cached_section_rejects_dynamic_placeholders() {
    let text = "date={{date}}".to_string();
    assert_eq!(
        validate_section_override(PromptSectionZone::Cached, &text),
        Err(OverrideRejection::CachedDynamicPlaceholder(
            "date".to_string()
        ))
    );
    // Cached 段没有占位符时正常接受
    assert!(validate_section_override(PromptSectionZone::Cached, "纯静态文本").is_ok());
}

#[test]
fn escaped_literal_braces_are_not_placeholders() {
    let text = "字面量 \\{{cwd}} 与 \\{{whatever}}";
    assert!(
        validate_section_override(PromptSectionZone::Cached, text).is_ok(),
        "转义后的花括号是字面量，不参与模板校验"
    );
}

// ─── L3：准入批处理 ─────────────────────────────────────────────────────

#[test]
fn sanitize_rejects_absent_target_and_bad_sections_keeps_valid() {
    let sections = vec![
        section("01_intro", PromptSectionZone::Cached, 1, "BUILTIN-INTRO"),
        section(
            "07_runtime",
            PromptSectionZone::Uncached,
            1,
            "BUILTIN-RUNTIME",
        ),
    ];
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert("01_intro".to_string(), Arc::from("{{date}}"));
    state
        .section_overrides
        .insert("07_runtime".to_string(), Arc::from("env: {{cwd}}"));
    state
        .section_overrides
        .insert("99_unknown".to_string(), Arc::from("inert"));

    let mut rejected = sanitize_section_overrides(&mut state, &sections);
    rejected.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        rejected,
        vec![
            (
                "01_intro".to_string(),
                OverrideRejection::CachedDynamicPlaceholder("date".to_string())
            ),
            ("99_unknown".to_string(), OverrideRejection::SectionAbsent),
        ],
        "非法覆盖与无目标覆盖被拒绝，合法覆盖保留"
    );
    assert!(state.section_overrides.contains_key("07_runtime"));
    assert_eq!(state.section_overrides.len(), 1);
}

#[test]
fn sanitize_enforces_total_budget_deterministically() {
    let sections: Vec<PromptSection> = (0..5)
        .map(|index| Box::leak(format!("s{index}").into_boxed_str()) as &'static str)
        .map(|id| section(id, PromptSectionZone::Uncached, 1, "BUILTIN"))
        .collect();
    let mut state = MetaHarnessState::default();
    let chunk = "x".repeat(MAX_SECTION_OVERRIDE_BYTES);
    for id in ["s0", "s1", "s2", "s3", "s4"] {
        state
            .section_overrides
            .insert(id.to_string(), Arc::from(chunk.as_str()));
    }

    let rejected = sanitize_section_overrides(&mut state, &sections);
    assert_eq!(rejected.len(), 1, "只是超总预算的那个被拒绝");
    assert_eq!(rejected[0].0, "s4", "按 id 排序确定性地拒绝最后一个");
    assert_eq!(rejected[0].1.category(), "total-budget");
    assert_eq!(state.section_overrides.len(), 4);
}

// ─── M11：装配守护 ─────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "段落装配冲突")]
fn duplicate_section_id_fails_at_construction() {
    let sections = vec![
        section("dup", PromptSectionZone::Uncached, 1, "FIRST"),
        section("dup", PromptSectionZone::Uncached, 2, "SECOND"),
    ];
    let _ = PromptTemplate::new(&MetaHarnessState::default(), &sections);
}

#[test]
#[should_panic(expected = "段落装配冲突")]
fn duplicate_zone_order_fails_at_construction() {
    let sections = vec![
        section("first", PromptSectionZone::Uncached, 1, "FIRST"),
        section("second", PromptSectionZone::Uncached, 1, "SECOND"),
    ];
    let _ = PromptTemplate::new(&MetaHarnessState::default(), &sections);
}

#[test]
#[should_panic(expected = "缓存区只接收纯静态模板")]
fn cached_section_with_placeholder_fails_at_construction() {
    let sections = vec![section(
        "cached_dyn",
        PromptSectionZone::Cached,
        1,
        "{{cwd}}",
    )];
    let _ = PromptTemplate::new(&MetaHarnessState::default(), &sections);
}

/// 不同 zone 的同一 order 合法（zone 是位置键的一部分）。
#[test]
fn same_order_in_different_zones_is_allowed() {
    let sections = vec![
        section("cached", PromptSectionZone::Cached, 1, "CACHED"),
        section("uncached", PromptSectionZone::Uncached, 1, "UNCACHED"),
    ];
    let rendered = PromptTemplate::new(&MetaHarnessState::default(), &sections).render(
        &PromptEnv::local_probe("/tmp", "2026-01-01"),
        &AgentCatalogProvider::new(),
    );
    assert!(rendered.starts_with("CACHED"));
    assert!(rendered.contains("UNCACHED"));
}

/// Cached 前缀字节稳定：env / catalog / date 变化不影响 boundary 前的静态区。
#[test]
fn cached_prefix_is_byte_stable_across_env_variations() {
    let sections = vec![
        section("cached", PromptSectionZone::Cached, 1, "STATIC-PREFIX"),
        section(
            "uncached",
            PromptSectionZone::Uncached,
            1,
            "Runtime: {{cwd}}",
        ),
    ];
    let catalog = AgentCatalogProvider::new();
    let first = PromptTemplate::new(&MetaHarnessState::default(), &sections)
        .render(&PromptEnv::local_probe("/a", "2026-01-01"), &catalog);
    let second = PromptTemplate::new(&MetaHarnessState::default(), &sections)
        .render(&PromptEnv::local_probe("/b", "2026-12-31"), &catalog);

    let prefix = |text: &str| {
        text.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .next()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(prefix(&first), prefix(&second), "Cached 前缀必须字节稳定");
    assert_eq!(prefix(&first), "STATIC-PREFIX");
}

/// 旧快照的非法覆盖在模板构造期被降级为内置段（不 panic、不自动改写旧正文）。
#[test]
fn invalid_override_from_old_snapshot_falls_back_to_builtin() {
    let sections = vec![section(
        "01_intro",
        PromptSectionZone::Cached,
        1,
        "BUILTIN-INTRO",
    )];
    let state = state_with_override("01_intro", "{{date}}");

    let rendered = PromptTemplate::new(&state, &sections).render(
        &PromptEnv::local_probe("/tmp", "2026-01-01"),
        &AgentCatalogProvider::new(),
    );
    assert!(rendered.contains("BUILTIN-INTRO"), "非法覆盖保留内置段");
    assert!(!rendered.contains("2026-01-01"), "非法覆盖未应用");
}

/// L3 字面量转义在渲染层还原为花括号（校验与渲染共用同一占位符表）。
#[test]
fn escaped_braces_render_as_literals() {
    let sections = vec![section(
        "07_custom",
        PromptSectionZone::Uncached,
        1,
        "模板示例：\\{{name}} 与真实 {{cwd}}",
    )];
    let rendered = PromptTemplate::new(&MetaHarnessState::default(), &sections).render(
        &PromptEnv::local_probe("/workspace", "2026-01-01"),
        &AgentCatalogProvider::new(),
    );
    assert!(
        rendered.contains("{{name}}"),
        "转义花括号渲染为字面量: {rendered}"
    );
    assert!(
        rendered.contains("真实 /workspace"),
        "已知占位符照常替换: {rendered}"
    );
}
