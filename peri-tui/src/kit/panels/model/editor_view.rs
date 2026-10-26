//! Model 面板的内联 editor / Responses 档位草稿渲染。
//!
//! 纯绘制：状态与动作分别由 `profile_draft` / `edit` 持有，避免组件主文件膨胀。

use super::profile_draft::{DraftFocus, ProfileDraftState, WindowEditState};
use crate::i18n;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::{
    style::Style,
    text::{Line, Span},
};

/// 右栏内联视图的鼠标语义目标；行与命中区来自同一次构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneHit {
    /// 普通右栏 K/V 行（值为 `FIELD_*` 索引）。
    Field(usize),
    Provider,
    Model,
    Window,
    Save,
    Cancel,
}

/// 普通右栏 K/V 行的命中表：行号即 `FIELD_*` 索引，与渲染行同序。
pub(super) fn kv_row_hits(field_count: usize) -> Vec<Option<PaneHit>> {
    (0..field_count)
        .map(|field| Some(PaneHit::Field(field)))
        .collect()
}

/// 右栏内联视图的行与逐行命中区（`hits` 与 `lines` 等长）。
pub(super) struct PaneLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) hits: Vec<Option<PaneHit>>,
}

impl PaneLayout {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            hits: Vec::new(),
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.hits.push(None);
    }

    fn push_hit(&mut self, line: Line<'static>, hit: PaneHit) {
        self.lines.push(line);
        self.hits.push(Some(hit));
    }
}

/// Responses contextWindow 内联 editor 的右栏内容。
pub(super) fn render_window_editor(
    editor: &WindowEditState,
    theme: &ThemeDefinition,
) -> PaneLayout {
    let mut out = PaneLayout::new();
    out.push(Line::from(Span::styled(
        i18n::tr_args(
            "model-panel-window-editing",
            &[(
                "alias".to_string(),
                fluent_bundle::FluentValue::from(editor.alias.as_str()),
            )],
        ),
        Style::new().fg(theme.semantic.status.warning).bold(),
    )));
    out.push(Line::from(""));
    out.push(Line::from(vec![
        Span::styled(
            format!("{}: ", i18n::tr("model-panel-context-window")),
            Style::new().fg(theme.semantic.text.muted),
        ),
        Span::styled(
            format!("{} · {}", editor.model, editor.provider_id),
            Style::new().fg(theme.semantic.text.primary),
        ),
    ]));
    let mut spans = vec![
        Span::styled("❯ ", Style::new().fg(theme.semantic.status.warning)),
        Span::styled(
            format!("{}: ", i18n::tr("model-panel-context-window")),
            Style::new()
                .fg(theme.component.panel.title)
                .add_modifier(ratatui_kit::ratatui::style::Modifier::BOLD),
        ),
    ];
    let chars: Vec<char> = editor.text.chars().collect();
    let cursor = editor.cursor.min(chars.len());
    for (index, ch) in chars.iter().enumerate() {
        spans.push(Span::styled(
            ch.to_string(),
            if index == cursor && cursor < chars.len() {
                Style::new()
                    .fg(theme.semantic.text.primary)
                    .bg(theme.semantic.status.warning)
            } else {
                Style::new().fg(theme.semantic.text.primary)
            },
        ));
    }
    if cursor >= chars.len() {
        spans.push(Span::styled(
            " ",
            Style::new().bg(theme.semantic.status.warning),
        ));
    }
    out.push(Line::from(spans));
    if let Some(error) = &editor.error {
        out.push(Line::from(Span::styled(
            format!("⚠ {error}"),
            Style::new().fg(theme.semantic.status.error),
        )));
        if editor.conflict {
            out.push(Line::from(Span::styled(
                i18n::tr("model-panel-conflict-reload-hint"),
                Style::new().fg(theme.semantic.text.dim),
            )));
        }
    }
    // 可见的保存/取消按钮：键盘 Enter/Esc 与鼠标点击走同一提交边界。
    out.push(Line::from(""));
    out.push_hit(
        draft_action_line("model-panel-draft-save", false, theme),
        PaneHit::Save,
    );
    out.push_hit(
        draft_action_line("model-panel-draft-cancel", false, theme),
        PaneHit::Cancel,
    );
    out
}

/// Responses provider/model/window 草稿的右栏内容。
pub(super) fn render_profile_draft(
    draft: &ProfileDraftState,
    theme: &ThemeDefinition,
) -> PaneLayout {
    let mut out = PaneLayout::new();
    out.push(Line::from(Span::styled(
        i18n::tr_args(
            "model-panel-draft-title",
            &[(
                "alias".to_string(),
                fluent_bundle::FluentValue::from(draft.alias.as_str()),
            )],
        ),
        Style::new().fg(theme.semantic.status.warning).bold(),
    )));
    out.push(Line::from(""));
    out.push_hit(
        draft_field_line(
            "model-panel-draft-provider",
            &draft.provider_id,
            draft.focus == DraftFocus::Provider,
            theme,
        ),
        PaneHit::Provider,
    );
    out.push_hit(
        if draft.model_editing {
            draft_model_line(draft, theme)
        } else {
            draft_field_line(
                "model-panel-draft-model",
                &draft.model,
                draft.focus == DraftFocus::Model,
                theme,
            )
        },
        PaneHit::Model,
    );
    out.push_hit(draft_window_line(draft, theme), PaneHit::Window);
    if let Some(error) = &draft.error {
        out.push(Line::from(Span::styled(
            format!("⚠ {error}"),
            Style::new().fg(theme.semantic.status.error),
        )));
        if draft.conflict {
            out.push(Line::from(Span::styled(
                i18n::tr("model-panel-conflict-reload-hint"),
                Style::new().fg(theme.semantic.text.dim),
            )));
        }
    }
    out.push(Line::from(""));
    out.push_hit(
        draft_action_line(
            "model-panel-draft-save",
            draft.focus == DraftFocus::Save,
            theme,
        ),
        PaneHit::Save,
    );
    out.push_hit(
        draft_action_line(
            "model-panel-draft-cancel",
            draft.focus == DraftFocus::Cancel,
            theme,
        ),
        PaneHit::Cancel,
    );
    out
}

fn draft_field_line(
    label_key: &str,
    value: &str,
    focused: bool,
    theme: &ThemeDefinition,
) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    let label_style = if focused {
        Style::new()
            .fg(theme.component.panel.title)
            .add_modifier(ratatui_kit::ratatui::style::Modifier::BOLD)
    } else {
        Style::new().fg(theme.semantic.text.muted)
    };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(theme.semantic.status.warning)),
        Span::styled(format!("{}: ", i18n::tr(label_key)), label_style),
        Span::styled(
            value.to_string(),
            Style::new().fg(theme.semantic.text.primary),
        ),
    ])
}

fn draft_action_line(label_key: &str, focused: bool, theme: &ThemeDefinition) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(theme.semantic.status.warning)),
        Span::styled(
            format!("[{}]", i18n::tr(label_key)),
            if focused {
                Style::new()
                    .fg(theme.component.panel.title)
                    .add_modifier(ratatui_kit::ratatui::style::Modifier::BOLD)
            } else {
                Style::new().fg(theme.semantic.text.primary)
            },
        ),
    ])
}

fn draft_model_line(draft: &ProfileDraftState, theme: &ThemeDefinition) -> Line<'static> {
    let mut spans = vec![
        Span::styled("❯ ", Style::new().fg(theme.semantic.status.warning)),
        Span::styled(
            format!("{}: ", i18n::tr("model-panel-draft-model")),
            Style::new()
                .fg(theme.component.panel.title)
                .add_modifier(ratatui_kit::ratatui::style::Modifier::BOLD),
        ),
    ];
    let chars: Vec<char> = draft.model.chars().collect();
    let cursor = draft.model_cursor.min(chars.len());
    for (index, ch) in chars.iter().enumerate() {
        spans.push(Span::styled(
            ch.to_string(),
            if index == cursor && cursor < chars.len() {
                Style::new()
                    .fg(theme.semantic.text.primary)
                    .bg(theme.semantic.status.warning)
            } else {
                Style::new().fg(theme.semantic.text.primary)
            },
        ));
    }
    if cursor >= chars.len() {
        spans.push(Span::styled(
            " ",
            Style::new().bg(theme.semantic.status.warning),
        ));
    }
    Line::from(spans)
}

fn draft_window_line(draft: &ProfileDraftState, theme: &ThemeDefinition) -> Line<'static> {
    let focused = draft.focus == DraftFocus::Window;
    let marker = if focused { "❯ " } else { "  " };
    let label_style = if focused {
        Style::new()
            .fg(theme.component.panel.title)
            .add_modifier(ratatui_kit::ratatui::style::Modifier::BOLD)
    } else {
        Style::new().fg(theme.semantic.text.muted)
    };
    let mut spans = vec![
        Span::styled(marker, Style::new().fg(theme.semantic.status.warning)),
        Span::styled(
            format!("{}: ", i18n::tr("model-panel-draft-context-window")),
            label_style,
        ),
    ];
    let chars: Vec<char> = draft.text.chars().collect();
    let cursor = draft.cursor.min(chars.len());
    for (index, ch) in chars.iter().enumerate() {
        spans.push(Span::styled(
            ch.to_string(),
            if focused && index == cursor {
                Style::new()
                    .fg(theme.semantic.text.primary)
                    .bg(theme.semantic.status.warning)
            } else {
                Style::new().fg(theme.semantic.text.primary)
            },
        ));
    }
    if focused && cursor >= chars.len() {
        spans.push(Span::styled(
            " ",
            Style::new().bg(theme.semantic.status.warning),
        ));
    }
    Line::from(spans)
}
