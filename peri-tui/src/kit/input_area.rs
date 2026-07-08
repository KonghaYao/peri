//! 输入区域 #[component] 组件。
//!
//! S8：完整输入体验——
//! - **多行 buffer**：Shift/Alt+Enter 换行；渲染按行拆分；高度动态扩展（3~40% 屏幕）
//! - **history**：Up/Down 浏览 `INPUT_HISTORY` atom；Esc 或回到栈底恢复编辑态
//! - **@mention**：输入 @ 触发 AT_MENTION_ACTIVE；popup 显示在输入框上方
//! - **slash**：行首 / 触发 SLASH_HINT_ACTIVE；popup 显示在输入框上方
//! - **提交**：Enter 提交，submit_consumer 消费 + push_history
//
// element! 宏展开为 `XxxProps { ... ..Default::default() }`，全字段已指定时
// clippy 触发 needless_update 警告。该警告来自宏展开而非用户代码，模块级抑制。
#![allow(clippy::needless_update)]

use peri_widgets::textarea::TextAreaState;

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};
use std::sync::{Arc, Mutex};

use parking_lot::RwLock;
use std::sync::OnceLock;

use crate::kit::atoms::PredictionState;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::atoms::{
    ACP_STATE, AT_MENTION_ACTIVE, AVAILABLE_SLASH_COMMANDS, FILE_LIST, INPUT_AREA_ESC_PREFIX,
    INPUT_BUFFER, MENTION_PREFIX, MENTION_SELECTED_INDEX, PREDICTION, RENDER_CACHE, SKILL_NAMES,
    SLASH_HINT_ACTIVE, SLASH_PREFIX, SLASH_SELECTED_INDEX, SUBMIT_TX, VIEW_MODELS,
};
use crate::kit::focus_router::input_accepts_key;
use crate::kit::input_history::{history_down, history_up, push_history, reset_history_cursor};
use crate::kit::mention_popup::MentionPopup;
use crate::kit::panel_registry::{PANELS, open_panel, panel_for_slash_command};
use crate::kit::render_bridge::{self, RenderedEntry, VmKey};
use crate::kit::slash_completion::{SlashActionKind, SlashCompletion, SlashCompletionItem};
use crate::kit::theme;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble, tui_hash_str};

/// 追踪 composer 段落区域，供鼠标点击→光标定位使用。
/// 仿照 MsgAreaTracker 模式：rect 是值类型，每帧 pre_component_draw 更新后在
/// handler 注册前取出副本传给闭包。
struct AreaTracker {
    rect: Option<Rect>,
}

impl Hook for AreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

/// 将终端显示列坐标转换为行内字符索引（处理 CJK 双宽字符）。
fn display_col_to_char_idx(line: &str, display_col: usize) -> usize {
    let mut col = 0usize;
    for (char_idx, ch) in line.chars().enumerate() {
        if col >= display_col {
            return char_idx;
        }
        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    line.chars().count()
}

#[derive(Default, Props)]
pub struct InputAreaProps {
    pub loading: bool,
    pub hidden: bool,
}

#[component]
pub fn InputArea(props: &InputAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 单一编辑状态——闭包编辑 + 渲染读取共享同一实例
    let state = hooks.use_state(TextAreaState::default);

    // 追踪 composer 区域 + overlay 高度，用于鼠标点击→光标定位
    // area_tracker: 值拷贝模式（仿 MsgAreaTracker），避免每帧 Arc 重建导致 handler 读到 None
    let composer_area;
    {
        let tracker = hooks.use_hook(|| AreaTracker { rect: None });
        composer_area = tracker.rect; // 每帧取副本，区块结束即释放 &mut hooks 借用
    }
    let overlay_height = Arc::new(parking_lot::Mutex::new(0u16));

    hooks.use_event_handler(
        EventScope::Current,
        EventPriority::Normal,
        move |event| match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if !input_accepts_key(&key) {
                    return EventResult::Ignored;
                }
                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let is_alt = key.modifiers.contains(KeyModifiers::ALT);

                if matches!(key.code, KeyCode::Esc) {
                    INPUT_AREA_ESC_PREFIX.set(is_alt);
                } else {
                    INPUT_AREA_ESC_PREFIX.set(false);
                }

                // 当前是否激活了 @mention / slash（激活时方向键给 popup 用）
                let mention_active = *AT_MENTION_ACTIVE.state().read();
                let slash_active = *SLASH_HINT_ACTIVE.state().read();

                let result = match key.code {
                    // ── 提交 ──（仅在不激活 popup 时按 Enter 提交）
                    KeyCode::Enter if !is_shift && !is_alt && !mention_active && !slash_active => {
                        let mut s = state.write();
                        let submitted = s.take_text();
                        drop(s);

                        submit_text(submitted);
                        reset_mention_popup();
                        reset_slash_popup();
                        *PREDICTION.state().write() = PredictionState::default();
                        EventResult::Consumed
                    }

                    // Shift/Alt+Enter：换行（多行 buffer）
                    KeyCode::Enter if (is_shift || is_alt) && !mention_active && !slash_active => {
                        state.write().insert_char('\n');
                        EventResult::Consumed
                    }

                    // ── 编辑快捷键 ──
                    KeyCode::Char('w') if is_ctrl => {
                        state.write().delete_word_backward();
                        EventResult::Consumed
                    }
                    KeyCode::Char('u') if is_ctrl => {
                        state.write().clear();
                        reset_mention_popup();
                        reset_slash_popup();
                        EventResult::Consumed
                    }
                    KeyCode::Char('a') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_line_home();
                        EventResult::Consumed
                    }
                    KeyCode::Char('e') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_line_end();
                        EventResult::Consumed
                    }
                    KeyCode::Char('b') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_left();
                        EventResult::Consumed
                    }
                    KeyCode::Char('f') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_right();
                        EventResult::Consumed
                    }
                    KeyCode::Char('h') if is_ctrl && !mention_active && !slash_active => {
                        let mut s = state.write();
                        s.backspace();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('d') if is_ctrl && !mention_active && !slash_active => {
                        let mut s = state.write();
                        s.delete_forward();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('z')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        let mut s = state.write();
                        s.undo();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('r')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        let mut s = state.write();
                        s.redo();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('y')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        let mut s = state.write();
                        s.paste_yank();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('b')
                        if is_alt && !is_ctrl && !mention_active && !slash_active =>
                    {
                        state.write().cursor_word_left();
                        EventResult::Consumed
                    }
                    KeyCode::Char('f')
                        if is_alt && !is_ctrl && !mention_active && !slash_active =>
                    {
                        state.write().cursor_word_right();
                        EventResult::Consumed
                    }
                    KeyCode::Backspace if !mention_active && !slash_active => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_backward();
                        } else {
                            s.backspace();
                        }
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Backspace => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_backward();
                        } else {
                            s.backspace();
                        }
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Delete if !mention_active && !slash_active => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_forward();
                        } else {
                            s.delete_forward();
                        }
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Left if is_alt && !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_word_left();
                        EventResult::Consumed
                    }
                    KeyCode::Right if is_alt && !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_word_right();
                        EventResult::Consumed
                    }

                    KeyCode::Left if !mention_active && !slash_active => {
                        state.write().cursor_left();
                        EventResult::Consumed
                    }
                    KeyCode::Right if !mention_active && !slash_active => {
                        state.write().cursor_right();
                        EventResult::Consumed
                    }

                    // ── history 导航（仅在不激活 popup 且无 Ctrl 修饰时）──
                    // I18-B：必须排除 Ctrl+Up/Down/Home/End——这些键留给 message_area 滚动。
                    // 事件现在分别由 InputArea / MessageArea 用 `use_event_handler` 注册，
                    // 因此这里显式避开 Ctrl+ 组合，避免与消息区滚动键冲突。
                    KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed up");
                        let moved = state.write().cursor_line_up();
                        if !moved {
                            let current = state.read().all_text();
                            if let Some(historical) = history_up(Some(&current)) {
                                state.write().replace_all_no_undo(historical);
                            }
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed down");
                        let moved = state.write().cursor_line_down();
                        if !moved && let Some(historical) = history_down() {
                            state.write().replace_all_no_undo(historical);
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Home if !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_home();
                        EventResult::Consumed
                    }
                    KeyCode::End if !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_end();
                        EventResult::Consumed
                    }
                    // I19-D：Esc 不再清空草稿——用户多行输入时误按 Esc 会丢失全部内容。
                    // 双击 Esc 由 event_handlers.rs 上层处理（触发 RewindPopup），
                    // 用户想清空输入框用 Ctrl+U。popup 激活时 Esc 由 event_handlers 关 popup。
                    KeyCode::Esc if !mention_active && !slash_active => EventResult::Ignored,

                    // ── 字符输入 ──
                    KeyCode::Char(ch) if !is_ctrl && !is_alt => {
                        let mut s = state.write();
                        s.insert_char(ch);
                        update_popup_prefix(&s);
                        *PREDICTION.state().write() = PredictionState::default();
                        EventResult::Consumed
                    }

                    // ── 预测文本接受（Tab）──
                    KeyCode::Tab => {
                        let pred = PREDICTION.state();
                        if !pred.read().text.is_empty() {
                            let text = pred.read().text.clone();
                            *pred.write() = PredictionState::default();
                            state.write().replace_all_no_undo(text);
                            reset_mention_popup();
                            reset_slash_popup();
                            return EventResult::Consumed;
                        }
                        EventResult::Ignored
                    }

                    _ => EventResult::Ignored,
                };

                if !is_alt {
                    INPUT_AREA_ESC_PREFIX.set(false);
                }
                result
            }
            Event::Paste(paste_text) => {
                // I22-A：paste 大小上限——防止用户误粘 10MB 日志冻结终端。
                // 10_000 chars 足够覆盖正常长 paste（代码片段、命令输出）；
                // 超出截断并 log warn 提示（用户可改用文件追加方式）。
                const MAX_PASTE_CHARS: usize = 10_000;
                // 部分终端（VSCode、iTerm2）在 Bracketed Paste 中使用 \r 作为
                // 换行分隔符；render_multiline_with_cursor 只按 \n 拆分行，
                // 未归一化的 \r 会导致换行在渲染时不可见。
                let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
                let char_count = normalized.chars().count();
                let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
                if char_count > MAX_PASTE_CHARS {
                    tracing::warn!(
                        original_chars = char_count,
                        capped_at = MAX_PASTE_CHARS,
                        "InputArea: paste 截断——超出 10K char 上限"
                    );
                }
                let mut s = state.write();
                s.insert_str(&truncated);
                update_popup_prefix(&s);
                *PREDICTION.state().write() = PredictionState::default();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        },
    );
    // ── 鼠标点击光标定位（Global scope，确保点击事件能到达）──
    {
        let state_cl = state.clone();
        let overlay_height_cl = overlay_height.clone();
        hooks.use_event_handler(
            ratatui_kit::prelude::EventScope::Global,
            ratatui_kit::prelude::EventPriority::High,
            move |event| {
                if let Event::Mouse(mouse) = event {
                    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                        return EventResult::Ignored;
                    }
                    if let Some(outer) = composer_area {
                        let ov_h = *overlay_height_cl.lock();
                        let composer_top = outer.y.saturating_add(ov_h).saturating_add(1);
                        let text_x = outer.x.saturating_add(3);
                        if mouse.row >= composer_top && mouse.column >= text_x {
                            let line_idx = mouse.row.saturating_sub(composer_top) as usize;
                            let display_col = mouse.column.saturating_sub(text_x) as usize;
                            let s = state_cl.read();
                            if !s.text.is_empty() {
                                let lines: Vec<&str> = s.text.split('\n').collect();
                                if line_idx < lines.len() {
                                    let char_col =
                                        display_col_to_char_idx(lines[line_idx], display_col);
                                    let new_cursor = TextAreaState::line_col_to_cursor(
                                        &s.text, line_idx, char_col,
                                    );
                                    drop(s);
                                    state_cl.write().cursor = new_cursor;
                                    // 点击在 composer 内，消费事件，阻止 message_area 误处理
                                    return EventResult::Consumed;
                                }
                            }
                            drop(s);
                        }
                    }
                }
                // 点击不在 composer 内，不消费
                EventResult::Ignored
            },
        );
    }
    let editor = state.read().clone();
    let hidden = props.hidden;
    let text = editor.text.clone();
    let cursor = editor.cursor;
    let loading = props.loading;

    // 当前激活状态（驱动 popup 渲染）
    let mention_active = *AT_MENTION_ACTIVE.state().read();
    let slash_active = *SLASH_HINT_ACTIVE.state().read();
    // 只在 popup 激活时才读/克隆 prefix 和 items，避免每帧不必要的 atom 读 + 堆分配
    let mention_prefix = if mention_active {
        MENTION_PREFIX.state().read().clone()
    } else {
        String::new()
    };
    let slash_prefix = if slash_active {
        SLASH_PREFIX.state().read().clone()
    } else {
        String::new()
    };

    // 多行渲染——按 \n 拆分，每行作为独立 Line，光标高亮放在对应行
    let lines = render_multiline_with_cursor_for_themed(&text, cursor, loading);

    // 计算 composer 本体高度；popup 额外占位，避免被输入区自身裁切。
    let line_count = text.matches('\n').count() + 1;
    let editor_rows = (line_count as u16).clamp(1, 10);
    let composer_height = editor_rows + 2;

    // 只在 slash 激活时克隆整个 item 列表——非激活态跳过 50+ item × 3 String 的堆分配
    let slash_items = if slash_active {
        get_cached_slash_items()
    } else {
        Vec::new()
    };
    let mention_select_state = state.clone();
    let slash_select_state = state.clone();

    let slash_popup_height = if slash_active {
        popup_height(slash_items.len())
    } else {
        0
    };
    // 只在 mention 激活时读 FILE_LIST + 过滤——非激活态跳过 200+ 文件名的 atom 读和分配
    let mention_items = if mention_active {
        filter_files_for_mention(&mention_prefix)
    } else {
        Vec::new()
    };
    let mention_popup_height = if mention_active {
        popup_height(mention_items.len())
    } else {
        0
    };
    let ov_height = slash_popup_height.max(mention_popup_height);
    *overlay_height.lock() = slash_popup_height.max(mention_popup_height);
    // 预测文本（buffer 为空且 prediction 非空时显示为灰色占位符）
    let pred_text = hooks.use_atom(&PREDICTION).read().text.clone();
    let show_prediction = !hidden && !pred_text.is_empty() && text.is_empty();
    let total_height = if hidden {
        0
    } else {
        composer_height + ov_height + if show_prediction { 1 } else { 0 }
    };

    let composer_lines = build_composer_lines(lines, loading);

    // 显式背景色：防止 Paragraph 文本缩短时旧内容残留（ghosting）。
    // 未设背景时 ratatui 仅渲染文本 span，超出新文本的列保留终端原有像素。
    let composer_paragraph = Paragraph::new(composer_lines)
        .block(build_composer_block(loading))
        .style(Style::default().bg(theme::semantic().surface.default));

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(total_height),
        ) {
            { if !hidden && slash_active {
                element!(SlashCompletion(
                    prefix: slash_prefix.clone(),
                    items: slash_items.clone(),
                    on_select: Arc::new(Mutex::new(Handler::from(move |item: SlashCompletionItem| {
                        match item.kind {
                            SlashActionKind::Panel => {
                                if let Some(kind) = panel_for_slash_command(&item.insert_text) {
                                    // 清空输入框再打开面板
                                    let mut editor = slash_select_state.write();
                                    editor.text.clear();
                                    editor.cursor = 0;
                                    open_panel(kind);
                                }
                            }
                            SlashActionKind::Command | SlashActionKind::Skill => {
                                // S16：command/skill 先检查是否映射到面板（如 /history → ThreadBrowser）
                                if let Some(kind) = panel_for_slash_command(&item.insert_text) {
                                    // 清空输入框再打开面板
                                    let mut editor = slash_select_state.write();
                                    editor.text.clear();
                                    editor.cursor = 0;
                                    open_panel(kind);
                                } else {
                                    let mut editor = slash_select_state.write();
                                    apply_slash_selection(&mut editor, &item.insert_text);
                                }
                            }
                        }
                        reset_slash_popup();
                    }))),
                    on_cancel: Arc::new(Mutex::new(Handler::from(|_: ()| {
                        reset_slash_popup();
                    }))),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            { if !hidden && mention_active {
                element!(MentionPopup(
                    prefix: mention_prefix.clone(),
                    items: mention_items.clone(),
                    on_select: Arc::new(Mutex::new(Handler::from(move |replacement: String| {
                        let mut editor = mention_select_state.write();
                        replace_last_mention(&mut editor, &replacement);
                        reset_mention_popup();
                    }))),
                    on_cancel: Arc::new(Mutex::new(Handler::from(|_: ()| {
                        reset_mention_popup();
                    }))),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            { if !hidden {
                element!(
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Length(composer_height),
                    ) {
                        Text(text: composer_paragraph)
                    }
                ).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            { if show_prediction {
                let pred_line = Line::from(Span::styled(
                    format!("  {}", pred_text),
                    Style::default().fg(theme::component().statusbar.muted),
                ));
                element!(
                    View(width: Constraint::Fill(1), height: Constraint::Length(1)) {
                        Text(text: Paragraph::new(pred_line))
                    }
                ).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
        }
    )
}

fn input_tokens() -> &'static theme::InputTokens {
    &theme::component().input
}

fn build_composer_block(loading: bool) -> Block<'static> {
    let tokens = input_tokens();
    let border_color = if loading {
        tokens.border_loading
    } else {
        tokens.border
    };

    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
}

fn build_composer_lines(editor_lines: Vec<Line<'static>>, loading: bool) -> Vec<Line<'static>> {
    let tokens = input_tokens();
    let mut lines = Vec::with_capacity(editor_lines.len().max(1));
    let prompt_style = Style::default()
        .fg(if loading {
            tokens.prompt_loading
        } else {
            tokens.prompt
        })
        .add_modifier(Modifier::BOLD);

    if editor_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ❯ ", prompt_style),
            Span::raw(""),
        ]));
        return lines;
    }

    for (index, line) in editor_lines.into_iter().enumerate() {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        if index == 0 {
            spans.push(Span::styled(" ❯ ", prompt_style));
        } else {
            spans.push(Span::styled(
                "   ",
                Style::default().fg(tokens.continuation),
            ));
        }
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

fn popup_height(item_count: usize) -> u16 {
    (item_count.max(1) as u16 + 2).min(theme::component().popup.inline_height)
}

fn submit_text(submitted: String) {
    if submitted.trim().is_empty() {
        return;
    }

    push_history(&submitted);
    reset_history_cursor();
    let trimmed = submitted.trim().to_string();
    let is_loading = ACP_STATE.state().read().is_loading;
    if is_loading {
        append_local_user_bubble(&trimmed);
        let input_buffer = INPUT_BUFFER.state();
        let mut guard = input_buffer.write();
        guard.push_back(trimmed);
        while guard.len() > 32 {
            guard.pop_front();
        }
    } else if let Some(tx) = SUBMIT_TX.get() {
        append_local_user_bubble(&trimmed);
        let _ = tx.send(trimmed);

        // S16：提交后立即设为 loading，避免按键到首条流式事件间的空白窗口期。
        let acp = ACP_STATE.state();
        let mut guard = acp.write();
        guard.is_loading = true;
    }
}

fn append_local_user_bubble(text: &str) {
    let user_vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: text.to_string(),
        content_hash: tui_hash_str(text),
        is_system_reminder: false,
    });
    let snapshot = {
        let vms = VIEW_MODELS.state();
        let snapshot = vms.read();
        let mut combined: Vec<TuiRenderUnit> = Vec::with_capacity(snapshot.committed.len() + 1);
        combined.extend(snapshot.committed.iter().cloned());
        combined.push(user_vm.clone());
        let new_snapshot = ViewModelsSnapshot {
            committed: Arc::from(combined),
            current_turn: Arc::clone(&snapshot.current_turn),
        };
        drop(snapshot);
        *vms.write() = new_snapshot.clone();
        new_snapshot
    };
    sync_render_cache(&snapshot);
}

fn sync_render_cache(snapshot: &ViewModelsSnapshot) {
    let width = 80;
    let mut cache = RENDER_CACHE.state().read().clone();

    // 增量模式：只追加最新一条 TuiUserBubble，避免全量 markdown 重解析
    if !cache.entries.is_empty() {
        if let Some(last_vm) = snapshot.committed.last() {
            let new_index = snapshot.committed.len().saturating_sub(1);
            let key = VmKey::Committed(new_index);
            let lines = crate::kit::view_render::render_v2_vm(last_vm, width);
            let height = render_bridge::visual_height(&lines, width);
            cache.entries.push((
                key,
                RenderedEntry {
                    height,
                    lines: Arc::from(lines),
                },
            ));
        }
    } else {
        // Fallback：cache 为空时走全量构建
        let mut entries =
            Vec::with_capacity(snapshot.committed.len() + snapshot.current_turn.len());
        append_render_entries(&mut entries, &snapshot.committed, width, 0, true);
        append_render_entries(&mut entries, &snapshot.current_turn, width, 0, false);
        cache.entries = entries;
    }

    // 重建 cumulative_heights
    cache.cumulative_heights.clear();
    let mut sum = 0usize;
    for (_, entry) in &cache.entries {
        sum = sum.saturating_add(entry.height);
        cache.cumulative_heights.push(sum);
    }
    let all_lines: Vec<ratatui::text::Line<'static>> = cache
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter())
        .cloned()
        .collect();
    cache.wrap_map = render_bridge::build_wrap_map(&all_lines, width as u16);
    *RENDER_CACHE.state().write() = cache;
}

fn append_render_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    items: &[TuiRenderUnit],
    width: usize,
    start_index: usize,
    committed: bool,
) {
    for (offset, vm) in items.iter().enumerate() {
        let key = if committed {
            VmKey::Committed(start_index + offset)
        } else {
            VmKey::CurrentTurn(offset)
        };
        let lines = crate::kit::view_render::render_v2_vm(vm, width);
        let height = render_bridge::visual_height(&lines, width);
        entries.push((
            key,
            RenderedEntry {
                height,
                lines: Arc::from(lines),
            },
        ));
    }
}

fn reset_mention_popup() {
    *AT_MENTION_ACTIVE.state().write() = false;
    MENTION_PREFIX.state().write().clear();
    *MENTION_SELECTED_INDEX.state().write() = 0;
}

fn reset_slash_popup() {
    *SLASH_HINT_ACTIVE.state().write() = false;
    SLASH_PREFIX.state().write().clear();
    *SLASH_SELECTED_INDEX.state().write() = 0;
}

fn replace_last_mention(state: &mut TextAreaState, replacement: &str) {
    if let Some(at_byte) = state.text.rfind('@') {
        let before = peri_widgets::textarea::History::snapshot(state);
        let after_at_byte = at_byte + 1;
        let keep_until_byte = state.text[after_at_byte..]
            .char_indices()
            .take_while(|(_, c)| !c.is_whitespace())
            .last()
            .map(|(i, c)| after_at_byte + i + c.len_utf8())
            .unwrap_or(after_at_byte);
        state.text.drain(after_at_byte..keep_until_byte);
        state.text.insert_str(after_at_byte, replacement);
        state.cursor = state.text.chars().count();
        state.record_edit(before);
    }
}

fn apply_slash_selection(state: &mut TextAreaState, cmd: &str) {
    let replacement = format!("/{cmd} ");
    if let Some((_, token_start_byte)) = detect_slash_token(&state.text, state.cursor_byte()) {
        let token_start = state.text[..token_start_byte].chars().count();
        let token_end = state.cursor;
        state.replace_char_range(token_start, token_end, &replacement);
    } else {
        state.replace_all(replacement);
    }
}

fn build_slash_items() -> Vec<SlashCompletionItem> {
    let remote = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    let skill_names: std::collections::HashSet<String> =
        SKILL_NAMES.state().read().iter().cloned().collect();
    let mut items = Vec::with_capacity(PANELS.len() + remote.len());
    for panel in PANELS {
        let slash_name = panel.slash_command.to_string();
        items.push(SlashCompletionItem {
            label_lowercase: slash_name.to_lowercase(),
            label: slash_name.clone(),
            insert_text: slash_name,
            description: panel.description.to_string(),
            kind: SlashActionKind::Panel,
        });
    }
    for (name, description) in &remote {
        // S16：根据 SKILL_NAMES 区分 Skill vs Command
        let kind = if skill_names.contains(name) {
            SlashActionKind::Skill
        } else {
            SlashActionKind::Command
        };
        items.push(SlashCompletionItem {
            label: name.clone(),
            insert_text: name.clone(),
            description: description.clone(),
            kind,
            label_lowercase: name.to_lowercase(),
        });
    }
    // 字母序排序——只排一次，组件端不再重排
    items.sort_by(|a, b| a.label_lowercase.cmp(&b.label_lowercase));
    items
}

/// 缓存 `build_slash_items()` 的结果，仅在 ACP 推送新命令时刷新。
static SLASH_ITEMS_CACHE: OnceLock<RwLock<Vec<SlashCompletionItem>>> = OnceLock::new();

fn slash_items_cache() -> &'static RwLock<Vec<SlashCompletionItem>> {
    SLASH_ITEMS_CACHE.get_or_init(|| RwLock::new(build_slash_items()))
}

/// 刷新斜杠命令缓存——由 acp_notifier 在收到新命令后调用。
pub(crate) fn refresh_slash_items() {
    *slash_items_cache().write() = build_slash_items();
}

fn get_cached_slash_items() -> Vec<SlashCompletionItem> {
    slash_items_cache().read().clone()
}

/// 从 `FILE_LIST` atom 读出 cwd 文件列表，按 `prefix` 过滤，最多 20 条。
///
/// 大小写不敏感的子串匹配——这样 `@auth` 能匹配 `auth.rs` / `oauth.rs` /
/// `authenticated.md` 等。结果按"prefix 开头优先"排序，提升命中率。
fn filter_files_for_mention(prefix: &str) -> Vec<String> {
    let files = FILE_LIST.state().read().clone();
    if prefix.is_empty() {
        return files.into_iter().take(20).collect();
    }
    let prefix_lower = prefix.to_lowercase();
    let mut matches: Vec<String> = files
        .iter()
        .filter(|f| f.to_lowercase().contains(&prefix_lower))
        .cloned()
        .collect();
    // prefix 开头的优先
    matches.sort_by_key(|f| !f.to_lowercase().starts_with(&prefix_lower));
    matches.truncate(20);
    matches
}

/// 根据 editor 当前文本和光标更新 @mention / slash 提示状态。
///
/// - `/` token：参考 peri-main，向光标前回溯最近的 `/`，要求 `/` 前为空白或行首。
/// - `@` 在最近词中：开启 @mention，prefix = @ 之后的字符。
fn update_popup_prefix(state: &TextAreaState) {
    let cursor_byte = state.cursor_byte();
    if let Some((prefix, _)) = detect_slash_token(&state.text, cursor_byte) {
        *SLASH_HINT_ACTIVE.state().write() = true;
        *SLASH_PREFIX.state().write() = prefix;
    } else {
        *SLASH_HINT_ACTIVE.state().write() = false;
        SLASH_PREFIX.state().write().clear();
    }

    let before_cursor = &state.text[..cursor_byte];
    let mention_active_now = if let Some(at_idx) = before_cursor.rfind('@') {
        let after = &before_cursor[at_idx + 1..];
        !after.is_empty() && !after.contains(char::is_whitespace) && after != "@"
    } else {
        false
    };
    *AT_MENTION_ACTIVE.state().write() = mention_active_now;
    if mention_active_now {
        if let Some(at_idx) = before_cursor.rfind('@') {
            *MENTION_PREFIX.state().write() = before_cursor[at_idx + 1..].to_string();
        }
    } else {
        MENTION_PREFIX.state().write().clear();
    }
}

/// 在 `text[..cursor_byte]` 中检测光标前最近的 `/` token。
fn detect_slash_token(text: &str, cursor_byte: usize) -> Option<(String, usize)> {
    if cursor_byte == 0 || cursor_byte > text.len() || !text.is_char_boundary(cursor_byte) {
        return None;
    }
    let before_cursor = &text[..cursor_byte];
    let slash_pos = before_cursor.rfind('/')?;
    let after_slash = &before_cursor[slash_pos + '/'.len_utf8()..];

    if slash_pos > 0 {
        let char_before = before_cursor[..slash_pos].chars().next_back()?;
        if !char_before.is_whitespace() {
            return None;
        }
    }

    if !after_slash.is_empty()
        && !after_slash
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
    {
        return None;
    }

    Some((after_slash.to_string(), slash_pos))
}

fn render_multiline_with_cursor_for_themed(
    text: &str,
    cursor: usize,
    loading: bool,
) -> Vec<ratatui::text::Line<'static>> {
    let tokens = input_tokens();
    let cursor_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::BOLD);
    peri_widgets::textarea::render_multiline_with_cursor(
        text,
        cursor,
        cursor_style,
        None,
        Style::default(),
        loading,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_apply_slash_selection_replaces_only_current_token() {
        let mut s = TextAreaState::default();
        s.insert_str("run /hel after");
        s.cursor = 8;
        apply_slash_selection(&mut s, "help");
        assert_eq!(s.text, "run /help  after");
        assert_eq!(s.cursor, 10);
    }

    #[test]
    fn test_apply_slash_selection_preserves_cjk_before_token() {
        let mut s = TextAreaState::default();
        s.insert_str("你好 /he 后面");
        s.cursor = 6;
        apply_slash_selection(&mut s, "help");
        assert_eq!(s.text, "你好 /help  后面");
        assert_eq!(s.cursor, 9);
    }

    #[test]
    fn test_detect_slash_token_rejects_path_or_comment() {
        assert!(detect_slash_token("src/foo", 7).is_none());
        assert!(detect_slash_token("//", 2).is_none());
    }

    #[test]
    fn test_detect_slash_token_accepts_line_start() {
        assert_eq!(
            detect_slash_token("hello\n/com", 10),
            Some(("com".to_string(), 6))
        );
    }

    fn reset_popup_atoms() {
        *AT_MENTION_ACTIVE.state().write() = false;
        *SLASH_HINT_ACTIVE.state().write() = false;
        MENTION_PREFIX.state().write().clear();
        SLASH_PREFIX.state().write().clear();
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_token_at_cursor() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = TextAreaState::default();
        s.insert_str("say /hel");
        update_popup_prefix(&s);
        assert!(!*AT_MENTION_ACTIVE.state().read());
        assert!(*SLASH_HINT_ACTIVE.state().read());
        assert_eq!(SLASH_PREFIX.state().read().as_str(), "hel");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_with_space_disables_after_token() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = TextAreaState::default();
        s.insert_str("/help me");
        update_popup_prefix(&s);
        assert!(!*SLASH_HINT_ACTIVE.state().read());
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_trigger() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = TextAreaState::default();
        s.insert_str("see @auth");
        update_popup_prefix(&s);
        assert!(*AT_MENTION_ACTIVE.state().read());
        assert_eq!(MENTION_PREFIX.state().read().as_str(), "auth");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = TextAreaState::default();
        s.insert_str("see @auth service");
        update_popup_prefix(&s);
        assert!(!*AT_MENTION_ACTIVE.state().read());
    }

    /// C2 回归测试：filter_files_for_mention 在 prefix 为空时返回前 20 条。
    #[test]
    #[serial]
    fn test_filter_files_empty_prefix_returns_top_20() {
        crate::kit::atoms::init_atoms();
        // 写 25 个文件
        {
            let state = FILE_LIST.state();
            let mut list = state.write();
            *list = (0..25).map(|i| format!("file{i}.rs")).collect();
            list.sort();
        }
        let result = filter_files_for_mention("");
        assert_eq!(result.len(), 20);
    }

    /// C2 回归测试：filter_files_for_mention 按大小写不敏感子串过滤。
    #[test]
    #[serial]
    fn test_filter_files_substring_case_insensitive() {
        crate::kit::atoms::init_atoms();
        *FILE_LIST.state().write() = vec![
            "auth.rs".into(),
            "oauth.rs".into(),
            "OAUTH.md".into(),
            "utils.rs".into(),
        ];
        let result = filter_files_for_mention("AUTH");
        // 三个含 auth/AUTH 的文件应被过滤出来（大小写不敏感）
        assert_eq!(result.len(), 3);
        assert!(result.contains(&"auth.rs".to_string()));
        assert!(result.contains(&"oauth.rs".to_string()));
        assert!(result.contains(&"OAUTH.md".to_string()));
    }

    /// C2 回归测试：prefix 开头的文件优先于子串匹配的。
    #[test]
    #[serial]
    fn test_filter_files_prefix_start_priority() {
        crate::kit::atoms::init_atoms();
        *FILE_LIST.state().write() = vec![
            "myauth.rs".into(), // 子串匹配
            "auth.rs".into(),   // 开头匹配，应优先
            "oauth.rs".into(),  // 子串匹配
        ];
        let result = filter_files_for_mention("auth");
        assert_eq!(result.first().unwrap(), "auth.rs");
    }
}
