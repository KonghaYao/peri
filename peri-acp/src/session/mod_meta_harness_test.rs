//! Frozen MetaHarness state and prompt behavior tests.

use super::*;

// ─── MetaHarness 冻结状态（设计 §2.3）───────────────────────────────────────

use std::collections::HashMap;

fn mh_cfg(entries: &[(&str, bool)]) -> HashMap<String, bool> {
    entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn default_state() -> peri_acp_types::meta_harness::MetaHarnessState {
    peri_acp_types::meta_harness::MetaHarnessState::default()
}

#[test]
fn build_meta_harness_state_empty_config_is_default() {
    let state = super::super::frozen::build_meta_harness_state(None, HashMap::new());
    assert_eq!(state, default_state());
    let state =
        super::super::frozen::build_meta_harness_state(Some(&HashMap::new()), HashMap::new());
    assert_eq!(state, default_state());
}

#[test]
fn build_meta_harness_state_can_disable_built_in_subagents() {
    let state = super::super::frozen::build_meta_harness_state(
        Some(&mh_cfg(&[("BuiltInSubagents", false)])),
        HashMap::new(),
    );
    assert!(!state.built_in_subagents_enabled);
}

#[test]
fn build_meta_harness_state_section_true_with_doc_enters_overrides() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "custom intro".to_string());
    let state =
        super::super::frozen::build_meta_harness_state(Some(&mh_cfg(&[("01_intro", true)])), docs);
    assert_eq!(
        state.section_overrides.get("01_intro").map(|s| s.as_ref()),
        Some("custom intro")
    );
    assert!(state.disabled_middlewares.is_empty());
}

#[test]
fn build_meta_harness_state_section_true_without_doc_warns_and_ignores() {
    let state = super::super::frozen::build_meta_harness_state(
        Some(&mh_cfg(&[("01_intro", true)])),
        HashMap::new(),
    );
    assert!(
        state.section_overrides.is_empty(),
        "文档缺失时忽略覆盖（保持内置段落）"
    );
}

#[test]
fn build_meta_harness_state_section_false_does_not_override() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "custom intro".to_string());
    let state =
        super::super::frozen::build_meta_harness_state(Some(&mh_cfg(&[("01_intro", false)])), docs);
    assert!(
        state.section_overrides.is_empty(),
        "section + false = 显式不覆盖，即使文档存在"
    );
}

#[test]
fn build_meta_harness_state_middleware_false_enters_disabled() {
    let state = super::super::frozen::build_meta_harness_state(
        Some(&mh_cfg(&[("WebMiddleware", false)])),
        HashMap::new(),
    );
    assert!(state.disabled_middlewares.contains("WebMiddleware"));
    assert!(state.section_overrides.is_empty());
}

#[test]
fn build_meta_harness_state_middleware_true_not_disabled() {
    let state = super::super::frozen::build_meta_harness_state(
        Some(&mh_cfg(&[("WebMiddleware", true)])),
        HashMap::new(),
    );
    assert!(
        state.disabled_middlewares.is_empty(),
        "middleware + true = 显式恢复装配"
    );
}

#[test]
fn build_meta_harness_state_mixed_entries() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "intro".to_string());
    docs.insert("05_using_tools".to_string(), "tools".to_string());
    let state = super::super::frozen::build_meta_harness_state(
        Some(&mh_cfg(&[
            ("01_intro", true),
            ("05_using_tools", false),
            ("WebMiddleware", false),
            ("WorkspaceMiddleware", true),
        ])),
        docs,
    );
    assert_eq!(state.section_overrides.len(), 1, "仅 true+文档存在 进入");
    assert!(state.section_overrides.contains_key("01_intro"));
    assert!(!state.section_overrides.contains_key("05_using_tools"));
    assert_eq!(state.disabled_middlewares.len(), 1);
    assert!(state.disabled_middlewares.contains("WebMiddleware"));
    assert!(!state.disabled_middlewares.contains("WorkspaceMiddleware"));
}

/// 集成：build_frozen_data 应用段落覆盖 + middleware 关闭集合到冻结载体；
/// 主 prompt 与 SubAgent 无 workflow prompt 共用同一覆盖。
///
/// J6：覆盖正文是**输入**（MCP 资源读取结果），不再由宿主扫描 `.peri/meta`。
#[tokio::test]
async fn test_build_frozen_data_applies_meta_harness_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();

    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![make_provider_config("a", "gpt-4o")];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    peri_config.config.meta_harness = Some(mh_cfg(&[
        ("01_intro", true),
        ("05_using_tools", true),
        ("WebMiddleware", false),
    ]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let config = Arc::new(peri_config);
    let mgr = SessionManager::new(
        session_resources,
        provider,
        Arc::clone(&config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        None,
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
    );

    let docs = HashMap::from([
        ("01_intro".to_string(), "CUSTOM-INTRO-BODY".to_string()),
        (
            "05_using_tools".to_string(),
            "CUSTOM-TOOLS-BODY".to_string(),
        ),
    ]);
    let frozen = mgr.build_frozen_data_with_config_and_runtime_and_docs(
        &config,
        &cwd,
        Some(&crate::prompt::PromptRuntimeEnv::detect(&cwd)),
        docs,
        // W4b（F3）：技能快照由内容准入期给定；本用例只覆盖段落覆盖面。
        &[],
        // W5：项目指令同样由内容准入期给定（本用例只覆盖段落覆盖面）。
        &Default::default(),
    );
    let state = frozen.meta_harness();
    assert_eq!(
        state.section_overrides.get("01_intro").map(|s| s.as_ref()),
        Some("CUSTOM-INTRO-BODY"),
        "冻结状态包含段落覆盖"
    );
    assert_eq!(
        state
            .section_overrides
            .get("05_using_tools")
            .map(|s| s.as_ref()),
        Some("CUSTOM-TOOLS-BODY")
    );
    assert!(
        state.disabled_middlewares.contains("WebMiddleware"),
        "冻结状态包含关闭集合"
    );
    // 主 prompt 应用覆盖（SubAgent / fork / workflow agent 直接复用主
    // prompt——子面向字段已随 C5 移除，无独立断言对象）
    assert!(
        frozen.system_prompt().contains("CUSTOM-INTRO-BODY"),
        "主 prompt 应用覆盖"
    );
    // accessor 与 v2_frozen 返回同一状态（单事实源）
    assert_eq!(
        frozen.meta_harness(),
        &frozen.v2_frozen().meta_harness,
        "accessor 与 FrozenContext 字段一致"
    );
}

/// 冻结语义（ARC-FROZEN-001 + J6 零 FS）：已构造的 frozen data 不随磁盘变化；
/// 覆盖正文只来自调用方给定的 docs，宿主从不读取 `.peri/meta`。
#[tokio::test]
async fn test_frozen_data_does_not_reread_meta_docs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    // 磁盘上放一份不同的正文：它不是事实源，宿主冻结不得读取它（X8 零 FS 兜底）。
    let meta_dir = std::path::Path::new(&cwd).join(".peri").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    std::fs::write(meta_dir.join("01_intro.md"), "DISK-BODY").unwrap();

    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![make_provider_config("a", "gpt-4o")];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    peri_config.config.meta_harness = Some(mh_cfg(&[("01_intro", true)]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let config = Arc::new(peri_config);
    let mgr = SessionManager::new(
        session_resources,
        provider,
        Arc::clone(&config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        None,
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
    );

    let v1 = HashMap::from([("01_intro".to_string(), "V1-BODY".to_string())]);
    let frozen = mgr.build_frozen_data_with_config_and_runtime_and_docs(
        &config,
        &cwd,
        Some(&crate::prompt::PromptRuntimeEnv::detect(&cwd)),
        v1,
        // W4b（F3）：技能快照由内容准入期给定；本用例只覆盖段落覆盖面。
        &[],
        &Default::default(),
    );
    assert!(frozen.system_prompt().contains("V1-BODY"));
    assert!(
        !frozen.system_prompt().contains("DISK-BODY"),
        "宿主冻结不得读取 .peri/meta 磁盘文件"
    );

    // 已构造的 frozen 不因新一轮输入或磁盘变化而变（ARC-FROZEN-001）。
    std::fs::remove_file(meta_dir.join("01_intro.md")).unwrap();
    assert!(
        frozen.system_prompt().contains("V1-BODY"),
        "已冻结的 prompt 不因磁盘变化而变（ARC-FROZEN-001）"
    );
    let v2 = HashMap::from([("01_intro".to_string(), "V2-BODY".to_string())]);
    let frozen2 = mgr.build_frozen_data_with_config_and_runtime_and_docs(
        &config,
        &cwd,
        Some(&crate::prompt::PromptRuntimeEnv::detect(&cwd)),
        v2,
        // W4b（F3）：技能快照由内容准入期给定；本用例只覆盖段落覆盖面。
        &[],
        &Default::default(),
    );
    assert!(
        frozen2.system_prompt().contains("V2-BODY"),
        "新会话（新 build）消费新给定的覆盖正文"
    );
    assert_eq!(
        frozen2
            .meta_harness()
            .section_overrides
            .get("01_intro")
            .map(|s| s.as_ref()),
        Some("V2-BODY")
    );
}
