//! HooksPanel 底部提示与真实动作的一致性回归。

/// 只读 Info 面板（333c51d4）：Enter 与 Esc 一样关闭面板，提示必须描述真实动作。
#[test]
fn test_hooks_nav_hint_describes_close_not_open() {
    let src = include_str!("hooks.rs");
    let main_src = src.split("#[cfg(test)]").next().expect("hooks module body");
    assert!(
        main_src.contains(r#"i18n::tr("panel-hooks-nav-hint")"#),
        "Hooks 面板底部提示须使用 Hooks 专属 key"
    );
    // 动作侧：Enter 分支（到下一个 key 分支为止）调用 close_panel
    let enter_arm = main_src
        .split("KeyCode::Enter")
        .nth(1)
        .and_then(|rest| rest.split("KeyCode::").next())
        .expect("hooks Enter 分支存在");
    assert!(
        enter_arm.contains("close_panel()"),
        "Hooks 面板 Enter 的真实动作是关闭面板；动作语义变化须同步提示与该用例"
    );
    for lang in ["en", "zh-CN"] {
        let hint = crate::i18n::LcRegistry::new(Some(lang)).tr("panel-hooks-nav-hint");
        assert!(
            !hint.contains("Enter::open"),
            "{lang} 提示不得把关闭伪装成打开：{hint}"
        );
        assert!(
            hint.contains("Enter::close") || hint.contains("Enter::关闭"),
            "{lang} 提示须标明 Enter 关闭：{hint}"
        );
    }
}
