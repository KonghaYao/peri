//! ConfigPanel beta flag 区块测试：行模型、显示值与切换写入/失败路径。
//!
//! 渲染输出本身不测（testing.md「不测 TUI 渲染输出」）；这里锁行模型派生、
//! 切换写入的候选快照与保存失败回滚。

use super::*;
use crate::kit::atoms::PERI_CONFIG_HANDLE;
use peri_acp_types::beta_flags::{BETA_FLAGS, BetaFlag, FULL_ASYNC_TOOLS};

/// 确保配置句柄存在（OnceLock：其他用例可能已建立，此处只保证非空）。
fn ensure_config_handle() -> std::sync::Arc<parking_lot::RwLock<crate::config::PeriConfig>> {
    let _ = PERI_CONFIG_HANDLE.set(std::sync::Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    )));
    std::sync::Arc::clone(PERI_CONFIG_HANDLE.get().expect("配置句柄必须存在"))
}

/// 行模型 = 基础行 + 区块标题行 + 注册表条目（顺序即注册表顺序）。
#[test]
fn test_row_model_appends_registry_driven_section() {
    let rows = config_rows();
    assert_eq!(rows.len(), CONFIG_ROWS.len() + 1 + BETA_FLAGS.len());
    for (index, row) in rows.iter().take(CONFIG_ROWS.len()).enumerate() {
        assert_eq!(*row, ConfigRow::Base(index), "基础行顺序必须保持");
    }
    assert_eq!(rows[CONFIG_ROWS.len()], ConfigRow::BetaSection);
    for (offset, flag) in BETA_FLAGS.iter().enumerate() {
        let row = rows[CONFIG_ROWS.len() + 1 + offset];
        let ConfigRow::BetaFlag(index) = row else {
            panic!("beta 区块必须逐条渲染 flag 行：{row:?}");
        };
        assert_eq!(BETA_FLAGS[index].id, flag.id, "区块顺序 = 注册表顺序");
    }
    // 鼠标命中与上下键导航都以行数为界：行数 = 行模型长度。
    assert_eq!(config_rows().len(), rows.len());
}

/// 区块标题行不可激活（光标/点击不改变任何配置）。
#[test]
#[serial_test::serial]
fn test_activate_section_row_is_inert() {
    let handle = ensure_config_handle();
    let before = handle.read().clone();
    let section_index = CONFIG_ROWS.len();
    activate_row(section_index, true);
    assert_eq!(*handle.read(), before, "区块标题行不得改写任何配置字段");
    // 越界行同样无副作用。
    activate_row(config_rows().len() + 10, true);
    assert_eq!(*handle.read(), before);
}

/// 描述优先 i18n key `beta-desc-<id>`，缺失回退注册表 canonical 文本。
///
/// 断言只用**缺失 key** 的形状（`beta-desc-definitely-missing-flag`）：翻译内容与
/// 语言无关，把它锁进期望会让「补/改翻译」变成测试失败。当前 `beta-desc-<id>` 的
/// 翻译覆盖不进入本用例（`fl`-bundle 命中路径由 i18n 自身测试覆盖）。
#[test]
fn test_beta_description_falls_back_to_canonical() {
    assert_eq!(
        beta_description("definitely-missing-flag", "canonical text"),
        "canonical text",
        "未命中 i18n key 时必须原样回退 canonical 文本"
    );
    // 注册表条目的 id 必须能作为 key 参与查找（不得 panic / 不得返回空）。
    let flag: &BetaFlag = &BETA_FLAGS[0];
    assert!(!beta_description(flag.id, flag.description).is_empty());
}

/// 面板显示生效层覆盖值：未设置与显式 false 均显示为关闭。
#[test]
#[serial_test::serial]
fn test_read_beta_toggle_reads_effective_layer_override() {
    let handle = ensure_config_handle();
    handle.write().config.betas.set(FULL_ASYNC_TOOLS, false);
    assert!(!read_beta_toggle(FULL_ASYNC_TOOLS), "显式 false 显示为关闭");
    handle
        .write()
        .config
        .betas
        .overrides
        .remove(FULL_ASYNC_TOOLS);
    assert!(!read_beta_toggle(FULL_ASYNC_TOOLS), "未设置显示为关闭");
    handle.write().config.betas.set(FULL_ASYNC_TOOLS, true);
    assert!(read_beta_toggle(FULL_ASYNC_TOOLS), "覆盖 true 显示为开启");
}

/// 切换：候选快照写入生效层键，经注入的保存入口持久化；成功后内存视图保持新值。
#[test]
#[serial_test::serial]
fn test_toggle_beta_flag_persists_candidate_snapshot() {
    let handle = ensure_config_handle();
    handle
        .write()
        .config
        .betas
        .overrides
        .remove(FULL_ASYNC_TOOLS);

    let saved = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let captured = std::sync::Arc::clone(&saved);
    toggle_beta_flag_with(FULL_ASYNC_TOOLS, move |candidate| {
        *captured.lock() = Some(
            candidate
                .config
                .betas
                .get(FULL_ASYNC_TOOLS)
                .unwrap_or(false),
        );
        Ok(())
    });

    assert_eq!(
        *saved.lock(),
        Some(true),
        "保存入口必须收到写入了 config.betas[id] 的候选快照"
    );
    assert!(read_beta_toggle(FULL_ASYNC_TOOLS), "成功后内存视图保持新值");
}

/// 保存失败路径：内存视图回滚到切换前的值，并留下保存失败提示。
#[test]
#[serial_test::serial]
fn test_toggle_beta_flag_save_failure_rolls_back_memory_view() {
    let handle = ensure_config_handle();
    handle.write().config.betas.set(FULL_ASYNC_TOOLS, false);

    toggle_beta_flag_with(FULL_ASYNC_TOOLS, |_| {
        Err(anyhow::anyhow!("configuration revision conflict"))
    });

    assert!(
        !read_beta_toggle(FULL_ASYNC_TOOLS),
        "保存失败必须保持内存视图不变（回滚本次写入）"
    );
    let notification = {
        let binding = NOTIFICATION.state();
        let state = binding.read();
        state
            .as_ref()
            .map(|notification| notification.message.clone())
    }
    .expect("保存失败必须提示错误");
    assert!(
        notification.contains("save failed"),
        "提示必须是保存失败：{notification}"
    );
    assert!(
        notification.contains("configuration revision conflict"),
        "提示必须携带失败原因：{notification}"
    );
}
