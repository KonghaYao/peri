//! PluginPanel 详情页底部提示与真实动作的一致性回归。

use crate::i18n::LcRegistry;

/// 详情页（installed detail / discover detail）Enter 执行所选动作、Esc 返回列表，
/// 提示不得沿用 `common-nav-enter-close` 的 `Enter::open` 描述。
#[test]
fn test_plugin_detail_footer_describes_execute_not_open() {
    let src = include_str!("../plugin.rs");
    assert!(
        src.contains(r#"i18n::tr("panel-plugin-detail-hint")"#),
        "Plugin 详情页底部提示须使用详情专属 key"
    );
    assert!(
        !src.contains("common-nav-enter-close"),
        "Plugin 详情页不得复用 Enter::open 语义的通用提示"
    );

    // 动作侧：详情模式 Enter 执行所选动作（区别于列表模式 Enter 进详情）
    let handler = include_str!("panel_handler.rs");
    let detail_enter_arm = handler
        .split("(true, false, KeyCode::Enter)")
        .nth(1)
        .and_then(|rest| rest.split("(true, false, KeyCode::").next())
        .expect("详情模式 Enter 分支存在");
    assert!(
        detail_enter_arm.contains("installed_action("),
        "Plugin 详情 Enter 的真实动作是执行所选动作；动作语义变化须同步提示与该用例"
    );
    assert!(
        include_str!("discover_handler.rs").contains("SearchEffect::Install"),
        "Discover 详情 Enter 的真实动作是执行安装/返回动作"
    );

    for lang in ["en", "zh-CN"] {
        let hint = LcRegistry::new(Some(lang)).tr("panel-plugin-detail-hint");
        assert!(
            !hint.contains("Enter::open"),
            "{lang} 提示不得声称打开详情：{hint}"
        );
        assert!(
            hint.contains("Enter: execute") || hint.contains("Enter: 执行"),
            "{lang} 提示须标明 Enter 执行所选动作：{hint}"
        );
    }
}

/// 列表态四个 tab 共用底部提示：Enter 进详情 / 激活添加市场输入，Esc 关闭面板。
/// Errors 页 Enter 会切回 Installed（`panel_handler.rs` 列表 Enter 分支末段），本行未描述。
#[test]
fn test_plugin_list_footer_describes_details_not_close() {
    let src = include_str!("../plugin.rs");
    assert!(
        src.contains(r#"i18n::tr("panel-plugin-list-hint")"#),
        "Plugin 列表态底部提示须使用列表专属 key"
    );
    assert!(
        !src.contains("common-nav-tab-close"),
        "Plugin 列表态不得复用 Enter close 语义的通用提示"
    );

    // 动作侧：列表模式 Enter 分支进详情 / 激活添加市场输入
    let handler = include_str!("panel_handler.rs");
    let list_enter_arm = handler
        .split("(false, false, KeyCode::Enter)")
        .nth(1)
        .and_then(|rest| rest.split("(false, false, KeyCode::").next())
        .expect("列表模式 Enter 分支存在");
    assert!(
        list_enter_arm.contains("detail_plugin_idx.write()"),
        "Plugin 列表 Enter 的真实动作是进详情；动作语义变化须同步提示与该用例"
    );
    assert!(
        list_enter_arm.contains("add_marketplace_active.write()"),
        "Marketplaces 首行 Enter 的真实动作是激活添加市场输入"
    );

    for lang in ["en", "zh-CN"] {
        let hint = LcRegistry::new(Some(lang)).tr("panel-plugin-list-hint");
        assert!(
            !hint.contains("Enter close") && !hint.contains("Enter::open"),
            "{lang} 提示不得声称 Enter 关闭或打开：{hint}"
        );
        assert!(
            hint.contains("Enter: details") || hint.contains("Enter: 查看详情"),
            "{lang} 提示须标明 Enter 查看详情：{hint}"
        );
    }
}
