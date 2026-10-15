//! Config 面板的写入与保存边界。
//!
//! 面板是宿主配置的编辑入口：写入目标恒为进程级固定的 `CONFIG_SOURCE_HANDLE`
//! （经 [`crate::config::save_effective`]），不随活动会话或活动工作区变化。
//!
//! 两条失败路径都必须**可见且不改内存**：
//! - 配置句柄缺失（进程初始化不完整）→ [`require_handle`] 提示 + 日志，动作不执行；
//! - 保存失败 → 提示失败原因，句柄保持已持久化的值，不发布本地成功。
//!
//! 因此每次写入都先落在 `PeriConfig` 副本上，落盘成功后才提交句柄（与 Login
//! 面板同一约定：先落盘、后发布）——面板显示的每个值都与磁盘一致。

use crate::config::{PeriConfig, TuiConfig};
use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification, PERI_CONFIG_HANDLE, TUI_CONFIG_HANDLE};
use fluent_bundle::FluentValue;
use peri_acp_types::beta_flags::BETA_FLAGS;
use std::sync::Arc;
use std::time::Duration;

use super::rows::{
    CONFIG_ROWS, ConfigRow, ROW_1M_CONTEXT, ROW_ACTIVE_ALIAS, ROW_CACHE_WARN, ROW_LANGUAGE,
    ROW_PERMISSION_MODE, ROW_SCROLL_FPS, ROW_SHOW_DIFF, ROW_STREAMING, RowType, config_rows,
    parse_permission_mode, permission_mode_label, read_cycle_idx,
};

/// 宿主配置句柄（`App::new` 建立一次，全进程共享）。
pub(super) type PeriHandle = Arc<parking_lot::RwLock<PeriConfig>>;
/// TUI 渲染配置句柄（只承载 UI 字段）。
pub(super) type TuiHandle = Arc<parking_lot::RwLock<TuiConfig>>;

/// 句柄名——失败提示中保留标识符原文（不翻译）。
const PERI_HANDLE: &str = "PERI_CONFIG_HANDLE";
const TUI_HANDLE: &str = "TUI_CONFIG_HANDLE";

// ---------------------------------------------------------------------------
// 失败与成功提示
// ---------------------------------------------------------------------------

fn notify(message: String, ttl: Duration) {
    *NOTIFICATION.state().write() = Some(Notification {
        message,
        until: peri_time::monotonic_now() + ttl,
    });
}

/// 保存成功提示。
fn notify_saved() {
    notify(i18n::tr("config-saved"), Duration::from_secs(1));
}

/// 保存失败提示：携带失败原因（面板既有的失败呈现机制）。
fn notify_save_failed(error: &anyhow::Error) {
    tracing::warn!(error = %error, "ConfigPanel: 配置保存失败");
    notify(
        i18n::tr_args(
            "config-save-failed",
            &[(
                "error".to_string(),
                FluentValue::from(error.to_string().as_str()),
            )],
        ),
        Duration::from_secs(2),
    );
}

/// 取一次性初始化的句柄：缺失时留下**可见**的失败记录并返回 `None`。
///
/// 正常生命周期内句柄在 `App::new` 之后必然存在；缺失意味着进程初始化不完整，
/// 此时静默返回会让用户以为操作已生效。
fn require_handle<'a, T>(handle: Option<&'a T>, name: &str) -> Option<&'a T> {
    if handle.is_none() {
        tracing::warn!(handle = name, "ConfigPanel: 配置句柄缺失，操作未执行");
        notify(
            i18n::tr_args(
                "config-handle-missing",
                &[("handle".to_string(), FluentValue::from(name))],
            ),
            Duration::from_secs(2),
        );
    }
    handle
}

/// 静态行表与实现不一致（行未接线）：如实报告失败，不静默保存未发生的写入。
fn unhandled_row(row: usize) {
    notify_save_failed(&anyhow::anyhow!(
        "config row {row} is not wired to an action"
    ));
}

// ---------------------------------------------------------------------------
// 候选与保存边界
// ---------------------------------------------------------------------------

/// 一次激活的待落盘内容：宿主配置副本 + 可选的 TUI 字段新值。
struct Candidate {
    peri: PeriConfig,
    tui: Option<(TuiHandle, TuiConfig)>,
}

impl Candidate {
    /// 以句柄当前值为起点构造候选——保存失败时句柄保持原值。
    fn from_handle(handle: &PeriHandle) -> Self {
        Self {
            peri: handle.read().clone(),
            tui: None,
        }
    }

    /// TUI 字段写入：写副本并把结果同步进候选的 `config.extra`（两者一起落盘）。
    ///
    /// 返回 `false` 表示 TUI 句柄缺失——失败提示已由 [`require_handle`] 给出。
    fn edit_tui(
        &mut self,
        handle: Option<&TuiHandle>,
        mutate: impl FnOnce(&mut TuiConfig),
    ) -> bool {
        let Some(handle) = require_handle(handle, TUI_HANDLE) else {
            return false;
        };
        let mut next = handle.read().clone();
        mutate(&mut next);
        next.sync_to_extra(&mut self.peri.config.extra);
        self.tui = Some((handle.clone(), next));
        true
    }

    /// 保存候选：成功才提交句柄（TUI 字段与宿主配置一起），失败只提示。
    ///
    /// 返回是否已持久化——调用方据此决定本地副作用（如语言注册表重建）是否执行。
    fn save(
        self,
        handle: &PeriHandle,
        save: impl FnOnce(&PeriConfig) -> anyhow::Result<()>,
    ) -> bool {
        let Self { peri, tui } = self;
        match save(&peri) {
            Ok(()) => {
                if let Some((tui_handle, tui)) = tui {
                    *tui_handle.write() = tui;
                }
                *handle.write() = peri;
                notify_saved();
                true
            }
            Err(error) => {
                notify_save_failed(&error);
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 行激活
// ---------------------------------------------------------------------------

/// 激活某行：toggle 反转，cycle forward=true 前进 / forward=false 后退。
///
/// 行模型分发：基础行沿用既有逐字段写入；beta flag 行写 `config.betas[id]`；
/// 区块标题行不可激活。
pub(super) fn activate_row(row: usize, forward: bool) {
    activate_row_with(
        PERI_CONFIG_HANDLE.get(),
        row,
        forward,
        crate::config::save_effective,
    );
}

/// 激活与持久化本体（句柄与保存入口注入）。
///
/// 注入只为测试：句柄缺失与保存失败两条路径都必须「不改内存、有可见提示」，
/// 而进程级 `OnceLock` 句柄在测试进程内无法制造缺失，保存也不应落到真实磁盘。
pub(super) fn activate_row_with(
    handle: Option<&PeriHandle>,
    row: usize,
    forward: bool,
    save: impl FnOnce(&PeriConfig) -> anyhow::Result<()>,
) {
    let Some(handle) = require_handle(handle, PERI_HANDLE) else {
        return;
    };
    match config_rows().get(row) {
        Some(ConfigRow::BetaFlag(flag_index)) => {
            toggle_beta_flag_with(Some(handle), BETA_FLAGS[*flag_index].id, save);
        }
        Some(ConfigRow::BetaSection) | None => {}
        Some(ConfigRow::Base(base_row)) => activate_base_row_with(handle, *base_row, forward, save),
    }
}

/// 基础行激活（索引 = `CONFIG_ROWS`）。
fn activate_base_row_with(
    handle: &PeriHandle,
    row: usize,
    forward: bool,
    save: impl FnOnce(&PeriConfig) -> anyhow::Result<()>,
) {
    let row_type = &CONFIG_ROWS[row].1;
    let mut candidate = Candidate::from_handle(handle);
    // 语言切换的本地副作用（重建 LcRegistry）只在落盘成功后执行：否则界面语言
    // 会与面板显示/磁盘不一致。
    let mut switched_language: Option<&'static str> = None;

    match row_type {
        RowType::Toggle => match row {
            ROW_SHOW_DIFF => {
                // TUI 字段：写副本 + 同步 extra，与 TUI 句柄一起提交。
                let edited = candidate.edit_tui(TUI_CONFIG_HANDLE.get(), |tui| {
                    tui.diff_enabled = !tui.diff_enabled;
                });
                if !edited {
                    return;
                }
            }
            ROW_CACHE_WARN => {
                let cur = candidate.peri.config.show_cache_warning.unwrap_or(false);
                candidate.peri.config.show_cache_warning = Some(!cur);
            }
            ROW_1M_CONTEXT => {
                let alias = candidate.peri.config.active_alias.clone();
                let Some(profile) = candidate.peri.config.profiles.get_mut(&alias) else {
                    // 活动档位不存在：没有任何字段可改，如实报告失败而非提示「已保存」。
                    notify_save_failed(&anyhow::anyhow!("active profile `{alias}` not found"));
                    return;
                };
                profile.context_1m = !profile.context_1m;
            }
            other => {
                unhandled_row(other);
                return;
            }
        },
        RowType::Cycle(options) => {
            let cur_idx = read_cycle_idx(row, options);
            let next = if forward {
                (cur_idx + 1) % options.len()
            } else {
                (cur_idx + options.len() - 1) % options.len()
            };
            let new_val = options[next];
            match row {
                ROW_STREAMING => {
                    let edited = candidate.edit_tui(TUI_CONFIG_HANDLE.get(), |tui| {
                        tui.streaming_mode = Some(new_val.to_string());
                    });
                    if !edited {
                        return;
                    }
                }
                ROW_LANGUAGE => {
                    candidate.peri.config.language = Some(new_val.to_string());
                    switched_language = Some(new_val);
                }
                ROW_ACTIVE_ALIAS => {
                    candidate.peri.config.active_alias = new_val.to_string();
                }
                // 权限模式改的是当前会话运行权限（session/set_mode），不写宿主持久配置。
                ROW_PERMISSION_MODE => {
                    if let Some(mode) = parse_permission_mode(new_val) {
                        crate::kit::permission_mode::request(permission_mode_label(mode));
                    }
                    return;
                }
                ROW_SCROLL_FPS => {
                    let fps = new_val.parse::<u32>().unwrap_or(20);
                    let edited = candidate.edit_tui(TUI_CONFIG_HANDLE.get(), |tui| {
                        tui.scroll_fps = Some(fps);
                    });
                    if !edited {
                        return;
                    }
                }
                other => {
                    unhandled_row(other);
                    return;
                }
            }
        }
    }

    if candidate.save(handle, save)
        && let Some(language) = switched_language
    {
        // i18n: 语言切换时重建 LcRegistry 并递增版本号，触发所有组件重渲染
        crate::i18n::switch(language);
    }
}

/// 切换 beta flag：候选副本上写当前生效层的 `config.betas[id]`，落盘成功才提交句柄。
///
/// 已运行会话不受影响（值随装配冻结，新区块标注「新会话生效」）。
pub(super) fn toggle_beta_flag_with(
    handle: Option<&PeriHandle>,
    id: &str,
    save: impl FnOnce(&PeriConfig) -> anyhow::Result<()>,
) {
    let Some(handle) = require_handle(handle, PERI_HANDLE) else {
        return;
    };
    let mut candidate = Candidate::from_handle(handle);
    let current = candidate.peri.config.betas.get(id).unwrap_or(false);
    candidate.peri.config.betas.set(id, !current);
    candidate.save(handle, save);
}
