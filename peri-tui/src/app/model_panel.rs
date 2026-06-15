use std::any::Any;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    Frame,
};
use tui_textarea::Input;

use super::{
    panel_component::PanelComponent,
    panel_manager::{EventResult, PanelContext, PanelKind},
    App,
};
use crate::config::{PeriConfig, ThinkingConfig};

// ─── AliasTab 枚举 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AliasTab {
    Opus,
    Sonnet,
    Haiku,
}

impl AliasTab {
    pub fn label(&self) -> &str {
        match self {
            Self::Opus => "Opus",
            Self::Sonnet => "Sonnet",
            Self::Haiku => "Haiku",
        }
    }

    pub fn to_key(&self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Opus => "Most capable for complex work",
            Self::Sonnet => "Balanced performance and speed",
            Self::Haiku => "Fastest for quick answers",
        }
    }
}

// ─── 行索引常量 ─────────────────────────────────────────────────────────────────

pub const ROW_OPUS: usize = 0;
pub const ROW_SONNET: usize = 1;
pub const ROW_HAIKU: usize = 2;
pub const ROW_MAX_TOKENS: usize = 3;
pub const ROW_EFFORT: usize = 4;
pub const ROW_1M_CONTEXT: usize = 5;
pub const ROW_COUNT: usize = 6;

// ─── ModelPanel ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ModelPanel {
    /// 当前激活 Provider 的显示名称
    pub provider_name: String,
    /// 当前选中的级别
    pub active_tab: AliasTab,
    /// Thinking effort 缓冲 "low" / "medium" / "high"
    pub buf_thinking_effort: String,
    /// max_tokens 值
    pub buf_max_tokens: u32,
    /// 1M 上下文开关
    pub buf_context_1m: bool,
    /// 光标所在行（0..ROW_COUNT-1）
    pub(crate) cursor: usize,
}

impl ModelPanel {
    pub fn from_config(cfg: &PeriConfig) -> Self {
        let active_tab = match cfg.config.active_alias.as_str() {
            "sonnet" => AliasTab::Sonnet,
            "haiku" => AliasTab::Haiku,
            _ => AliasTab::Opus,
        };

        let provider_name = cfg
            .config
            .providers
            .iter()
            .find(|p| p.id == cfg.config.active_provider_id)
            .map(|p| p.display_name().to_string())
            .unwrap_or_default();

        let cursor = match active_tab {
            AliasTab::Opus => ROW_OPUS,
            AliasTab::Sonnet => ROW_SONNET,
            AliasTab::Haiku => ROW_HAIKU,
        };

        let effort = cfg
            .config
            .thinking
            .as_ref()
            .map(|t| t.effort.clone())
            .unwrap_or_else(|| "high".to_string());

        let max_tokens = cfg
            .config
            .thinking
            .as_ref()
            .map(|t| t.max_tokens)
            .unwrap_or(32000);

        let context_1m = cfg.config.context_1m.unwrap_or(false);

        Self {
            provider_name,
            active_tab,
            buf_thinking_effort: effort,
            buf_max_tokens: max_tokens,
            buf_context_1m: context_1m,
            cursor,
        }
    }

    /// 光标位置
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 循环切换 effort：low → medium → high → xhigh → max → low（任意光标位置可切换）
    pub fn cycle_effort(&mut self, reverse: bool) {
        if reverse {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "max".to_string(),
                "max" => "xhigh".to_string(),
                "xhigh" => "high".to_string(),
                "high" => "medium".to_string(),
                _ => "low".to_string(),
            };
        } else {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "medium".to_string(),
                "medium" => "high".to_string(),
                "high" => "xhigh".to_string(),
                "xhigh" => "max".to_string(),
                _ => "low".to_string(),
            };
        }
    }

    /// max_tokens 预设值：8000 → 16000 → 32000 → 64000 → 128000 → 8000
    const MAX_TOKENS_PRESETS: &[u32] = &[8000, 16000, 32000, 64000, 128000];

    /// 循环切换 max_tokens 预设值
    pub fn cycle_max_tokens(&mut self, reverse: bool) {
        let current = self.buf_max_tokens;
        let presets = Self::MAX_TOKENS_PRESETS;
        if let Some(pos) = presets.iter().position(|&v| v == current) {
            if reverse {
                let next = if pos == 0 { presets.len() - 1 } else { pos - 1 };
                self.buf_max_tokens = presets[next];
            } else {
                let next = (pos + 1) % presets.len();
                self.buf_max_tokens = presets[next];
            }
        } else {
            // 非预设值回退到最近的预设值
            let pos = presets
                .partition_point(|&v| v < current)
                .min(presets.len() - 1);
            if reverse {
                self.buf_max_tokens = presets[pos.saturating_sub(1)];
            } else {
                self.buf_max_tokens = presets[pos];
            }
        }
    }

    /// 将面板状态写入 PeriConfig（alias + thinking + max_tokens + 1M context）
    pub fn apply_to_config(&self, cfg: &mut PeriConfig) {
        cfg.config.active_alias = self.active_tab.to_key().to_string();
        let t = cfg.config.thinking.get_or_insert_with(|| ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: self.buf_thinking_effort.clone(),
            max_tokens: self.buf_max_tokens,
        });
        t.enabled = true;
        t.effort = self.buf_thinking_effort.clone();
        t.max_tokens = self.buf_max_tokens;
        cfg.config.context_1m = Some(self.buf_context_1m);
    }
}

// ─── PanelComponent 实现 ──────────────────────────────────────────────────────

impl PanelComponent for ModelPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Model
    }

    fn handle_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;
        match input {
            Input { key: Key::Esc, .. } => EventResult::ClosePanel,
            Input { key: Key::Up, .. } => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                EventResult::Consumed
            }
            Input { key: Key::Down, .. } => {
                if self.cursor < ROW_COUNT - 1 {
                    self.cursor += 1;
                }
                EventResult::Consumed
            }
            Input {
                key: Key::Enter, ..
            } => match self.cursor() {
                ROW_OPUS => {
                    self.active_tab = AliasTab::Opus;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_SONNET => {
                    self.active_tab = AliasTab::Sonnet;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_HAIKU => {
                    self.active_tab = AliasTab::Haiku;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_EFFORT => {
                    self.cycle_effort(false);
                    EventResult::Consumed
                }
                ROW_MAX_TOKENS => {
                    self.cycle_max_tokens(false);
                    EventResult::Consumed
                }
                ROW_1M_CONTEXT => {
                    self.buf_context_1m = !self.buf_context_1m;
                    ModelPanel::apply_1m_context(self, ctx);
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            },
            // Space: 切换 effort 等级（无需选中 effort 行）或 max_tokens 或 1M 上下文
            Input {
                key: Key::Char(' '),
                ..
            } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(false);
                    EventResult::Consumed
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    ModelPanel::apply_1m_context(self, ctx);
                    EventResult::Consumed
                } else {
                    self.cycle_effort(false);
                    EventResult::Consumed
                }
            }
            // ←/→: 随时切换 effort 等级或 max_tokens 或 1M 上下文
            Input { key: Key::Left, .. } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(true);
                    EventResult::Consumed
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    ModelPanel::apply_1m_context(self, ctx);
                    EventResult::Consumed
                } else {
                    self.cycle_effort(true);
                    EventResult::Consumed
                }
            }
            Input {
                key: Key::Right, ..
            } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(false);
                    EventResult::Consumed
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    ModelPanel::apply_1m_context(self, ctx);
                    EventResult::Consumed
                } else {
                    self.cycle_effort(false);
                    EventResult::Consumed
                }
            }
            _ => EventResult::Consumed,
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        ctx: &mut PanelContext<'_>,
    ) -> EventResult {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            // border_top=1，计算点击的行索引
            let relative_y = mouse.row.saturating_sub(area.y);
            if relative_y >= 1 {
                let clicked = (relative_y - 1) as usize;
                if clicked < ROW_COUNT {
                    self.cursor = clicked;
                    return self.handle_key(
                        Input::from(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                        ctx,
                    );
                }
            }
        }
        EventResult::NotConsumed
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        13
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::model::render_model_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("↑↓".to_string(), _lc.tr("key-move")),
            ("Enter".to_string(), _lc.tr("key-confirm")),
            ("←→/Space".to_string(), _lc.tr("key-effort")),
            ("Esc".to_string(), _lc.tr("key-close")),
        ]
    }
}

impl ModelPanel {
    /// 将面板状态写入 config，推送系统消息，更新 provider/model 名称
    fn apply_and_close(panel: &ModelPanel, ctx: &mut PanelContext<'_>) {
        let alias_label = panel.active_tab.label().to_string();
        let effort = panel.buf_thinking_effort.clone();

        let cfg_arc = ctx.services.peri_config.clone();
        let mut cfg = cfg_arc.write();
        panel.apply_to_config(&mut cfg);

        let effort_display = match effort.as_str() {
            "low" => "Low",
            "high" => "High",
            "xhigh" => "XHigh",
            "max" => "Max",
            _ => "Medium",
        };

        ctx.session_mgr
            .current_mut()
            .messages
            .push_system_note(ctx.services.lc.tr_args(
                "app-model-switched",
                &[
                    ("alias".into(), alias_label.into()),
                    ("effort".into(), effort_display.into()),
                ],
            ));

        if let Err(e) = App::save_config(&cfg, ctx.services.config_path_override.as_deref()) {
            ctx.session_mgr
                .current_mut()
                .messages
                .push_system_note(ctx.services.lc.tr_args(
                    "app-config-save-failed",
                    &[("error".into(), e.to_string().into())],
                ));
        }

        if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
            ctx.services.provider_name = p.display_name().to_string();
            ctx.services.model_name = p.model_name().to_string();

            // 同步 context_window 到 TUI 状态（agent.context_window 用于 status line 显示）
            let mut cw = p.context_window();
            if panel.buf_context_1m {
                cw = 1_000_000;
            }
            if cw > 0 {
                ctx.session_mgr.current_mut().agent.context_window = cw;
            }
        }

        // 通过 ACP 协议同步到 Server
        let alias = cfg.config.active_alias.clone();
        drop(cfg);
        if let Some(ref acp_client) = ctx.acp_client {
            let acp = acp_client.clone();
            let effort = panel.buf_thinking_effort.clone();
            let context_1m_val = panel.buf_context_1m.to_string();
            tokio::spawn(async move {
                let _ = acp.set_config_option("model", &alias).await;
                let _ = acp.set_config_option("thinking_effort", &effort).await;
                let _ = acp.set_config_option("context_1m", &context_1m_val).await;
            });
        }
    }

    /// 即时应用 1M 上下文开关（不关闭面板）
    fn apply_1m_context(panel: &ModelPanel, ctx: &mut PanelContext<'_>) {
        let cfg_arc = ctx.services.peri_config.clone();
        let mut cfg = cfg_arc.write();
        cfg.config.context_1m = Some(panel.buf_context_1m);

        if panel.buf_context_1m {
            ctx.session_mgr
                .current_mut()
                .messages
                .push_system_note(ctx.services.lc.tr("app-1m-context-enabled"));
        }

        if let Err(e) = App::save_config(&cfg, ctx.services.config_path_override.as_deref()) {
            ctx.session_mgr
                .current_mut()
                .messages
                .push_system_note(ctx.services.lc.tr_args(
                    "app-config-save-failed",
                    &[("error".into(), e.to_string().into())],
                ));
        }

        // 同步 context_window 到 TUI 状态
        if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
            let mut cw = p.context_window();
            if panel.buf_context_1m {
                cw = 1_000_000;
            }
            if cw > 0 {
                ctx.session_mgr.current_mut().agent.context_window = cw;
            }
        }

        // 通过 ACP 协议同步到 Server
        if let Some(ref acp_client) = ctx.acp_client {
            let acp = acp_client.clone();
            let val = panel.buf_context_1m.to_string();
            tokio::spawn(async move {
                let _ = acp.set_config_option("context_1m", &val).await;
            });
        }
    }
}

#[cfg(test)]
#[path = "model_panel_test.rs"]
mod tests;
