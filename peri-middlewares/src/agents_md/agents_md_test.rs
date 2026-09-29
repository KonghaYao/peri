// `AgentsMdMiddleware` 的 adapter 证据（W5）：只做冻结内容的合成与贡献，
// 不含任何文件系统行为。
//
// 候选优先级、`@import` 深度/环防护、`CLAUDE.local.md` 叠加与 excludes 的
// 内容级语义已随读盘整体归 provider（`mcp-packages/workspace/src/resources/
// instructions_test.rs` 覆盖）；本文件的职责边界是「给定冻结正文 → 贡献文本」。


#[test]
fn no_frozen_content_contributes_nothing() {
    let mw = AgentsMdMiddleware::new();
    assert_eq!(contribution(&mw), None);
    assert!(mw.prompt_contribution().is_none());
}

#[test]
fn fresh_middleware_is_not_contributed() {
    // 空 hook 不再读盘：即使 state 有 cwd，before_agent 也不产生贡献。
    let mw = AgentsMdMiddleware::new();
    assert!(contribution(&mw).is_none());
}

#[test]
fn frozen_main_only_is_contributed_verbatim() {
    let mw = AgentsMdMiddleware::new().with_frozen_content("# Project rules".to_string(), None);
    assert_eq!(contribution(&mw).as_deref(), Some("# Project rules"));
}

#[test]
fn frozen_local_is_appended_after_blank_line() {
    let mw = AgentsMdMiddleware::new().with_frozen_content(
        "main body".to_string(),
        Some("local body".to_string()),
    );
    assert_eq!(
        contribution(&mw).as_deref(),
        Some("main body\n\nlocal body"),
        "main 与 local 之间以空行分隔（迁移前合成口径）"
    );
}

#[test]
fn blank_local_is_not_appended() {
    let mw = AgentsMdMiddleware::new()
        .with_frozen_content("main body".to_string(), Some("   \n\t ".to_string()));
    assert_eq!(contribution(&mw).as_deref(), Some("main body"));
}

#[test]
fn blank_main_with_local_contributes_local_only() {
    let mw = AgentsMdMiddleware::new()
        .with_frozen_content("   ".to_string(), Some("local body".to_string()));
    assert_eq!(contribution(&mw).as_deref(), Some("   \n\nlocal body"));
}

#[test]
fn all_blank_content_contributes_nothing() {
    let mw = AgentsMdMiddleware::new()
        .with_frozen_content("  \n".to_string(), Some(" \t".to_string()));
    assert_eq!(contribution(&mw), None);
}

#[test]
fn name_is_the_meta_harness_key() {
    let mw = AgentsMdMiddleware::new();
    assert_eq!(Middleware::name(&mw), "AgentsMdMiddleware");
}
