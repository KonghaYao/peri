//! ConfigPanel beta flag 区块测试：行模型、显示值与切换写入/失败路径。
//!
//! 渲染输出本身不测（testing.md「不测 TUI 渲染输出」）；这里锁行模型派生、
//! 切换写入的候选快照与保存失败回滚。
//!
//! 当前 ACP 注册表为空（`full-async-tools` 已转正删除），面板不渲染空区块；
//! 切换机制仍用 TUI 本地 beta 键验证，保证新增 flag 时行模型与写入路径可用。

use super::*;
use crate::kit::atoms::{NOTIFICATION, PERI_CONFIG_HANDLE};
use peri_acp_types::beta_flags::BETA_FLAGS;
use peri_config::betas::HISTORY_WINDOW_V1;

/// 确保配置句柄存在（OnceLock：其他用例可能已建立，此处只保证非空）。
pub(super) fn ensure_config_handle()
-> std::sync::Arc<parking_lot::RwLock<crate::config::PeriConfig>> {
    let _ = PERI_CONFIG_HANDLE.set(std::sync::Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    )));
    std::sync::Arc::clone(PERI_CONFIG_HANDLE.get().expect("配置句柄必须存在"))
}

/// 行模型 = 基础行 + 注册表条目；注册表为空时不渲染区块标题。
#[test]
fn test_row_model_matches_registry() {
    let rows = config_rows();
    let expected = CONFIG_ROWS.len() + if BETA_FLAGS.is_empty() { 0 } else { 1 } + BETA_FLAGS.len();
    assert_eq!(rows.len(), expected);
    for (index, row) in rows.iter().take(CONFIG_ROWS.len()).enumerate() {
        assert_eq!(*row, ConfigRow::Base(index), "基础行顺序必须保持");
    }
    if BETA_FLAGS.is_empty() {
        assert!(
            !rows.contains(&ConfigRow::BetaSection),
            "空注册表不得渲染 beta 区块"
        );
    } else {
        assert_eq!(rows[CONFIG_ROWS.len()], ConfigRow::BetaSection);
        for (offset, flag) in BETA_FLAGS.iter().enumerate() {
            let row = rows[CONFIG_ROWS.len() + 1 + offset];
            let ConfigRow::BetaFlag(index) = row else {
                panic!("beta 区块必须逐条渲染 flag 行：{row:?}");
            };
            assert_eq!(BETA_FLAGS[index].id, flag.id, "区块顺序 = 注册表顺序");
        }
    }
    // 鼠标命中与上下键导航都以行数为界：行数 = 行模型长度。
    assert_eq!(config_rows().len(), rows.len());
}

/// 描述优先 i18n key `beta-desc-<id>`，缺失回退注册表 canonical 文本。
///
/// 断言只用**缺失 key** 的形状（`beta-desc-definitely-missing-flag`）：翻译内容与
/// 语言无关，把它锁进期望会让「补/改翻译」变成测试失败。
#[test]
fn test_beta_description_falls_back_to_canonical() {
    assert_eq!(
        beta_description("definitely-missing-flag", "canonical text"),
        "canonical text",
        "未命中 i18n key 时必须原样回退 canonical 文本"
    );
}

/// 面板显示生效层覆盖值：未设置与显式 false 均显示为关闭。
#[test]
#[serial_test::serial]
fn test_read_beta_toggle_reads_effective_layer_override() {
    let handle = ensure_config_handle();
    handle.write().config.betas.set(HISTORY_WINDOW_V1, false);
    assert!(
        !read_beta_toggle(HISTORY_WINDOW_V1),
        "显式 false 显示为关闭"
    );
    handle
        .write()
        .config
        .betas
        .overrides
        .remove(HISTORY_WINDOW_V1);
    assert!(!read_beta_toggle(HISTORY_WINDOW_V1), "未设置显示为关闭");
    handle.write().config.betas.set(HISTORY_WINDOW_V1, true);
    assert!(read_beta_toggle(HISTORY_WINDOW_V1), "覆盖 true 显示为开启");
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
        .remove(HISTORY_WINDOW_V1);

    let saved = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let captured = std::sync::Arc::clone(&saved);
    super::toggle_beta_flag_with(
        PERI_CONFIG_HANDLE.get(),
        HISTORY_WINDOW_V1,
        move |candidate| {
            *captured.lock() = Some(
                candidate
                    .config
                    .betas
                    .get(HISTORY_WINDOW_V1)
                    .unwrap_or(false),
            );
            Ok(())
        },
    );

    assert_eq!(
        *saved.lock(),
        Some(true),
        "保存入口必须收到写入了 config.betas[id] 的候选快照"
    );
    assert!(
        super::read_beta_toggle(HISTORY_WINDOW_V1),
        "成功后内存视图保持新值"
    );
}

/// 保存失败路径：内存视图回滚到切换前的值，并留下保存失败提示。
#[test]
#[serial_test::serial]
fn test_toggle_beta_flag_save_failure_rolls_back_memory_view() {
    let handle = ensure_config_handle();
    handle.write().config.betas.set(HISTORY_WINDOW_V1, false);

    super::toggle_beta_flag_with(PERI_CONFIG_HANDLE.get(), HISTORY_WINDOW_V1, |_| {
        Err(anyhow::anyhow!("configuration revision conflict"))
    });

    assert!(
        !super::read_beta_toggle(HISTORY_WINDOW_V1),
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
