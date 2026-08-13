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

use crate::components::textarea::{TextAreaState, wrap_text};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};
use std::sync::{Arc, Mutex};

use parking_lot::RwLock;
use std::sync::OnceLock;

use crate::i18n;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::atoms::PredictionState;
use crate::kit::atoms::{
    ACP_STATE, ACTIVE_PANEL, AT_MENTION_ACTIVE, AVAILABLE_SLASH_COMMANDS, CONTEXT_USAGE,
    CURRENT_SESSION_TITLE, FILE_LIST, FOCUSED_ENTRY, INPUT_AREA_ESC_PREFIX, INPUT_BUFFER,
    LANG_VERSION, LOCAL_EVENT_TX, MCP_SKILL_NAMES, MENTION_PREFIX, MENTION_SELECTED_INDEX,
    PENDING_ATTACHMENTS, POPUP_KIND, PREDICTION, SERVICE_SNAPSHOT, SKILL_NAMES, SLASH_HINT_ACTIVE,
    SLASH_PREFIX, SLASH_SELECTED_INDEX, SUBMIT_TX, WIZARD_ACTIVE,
};
use crate::kit::focus_router::input_accepts_key;
use crate::kit::input_history::{history_down, history_up, push_history, reset_history_cursor};
use crate::kit::mention_popup::MentionPopup;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::mouse_router;
use crate::kit::panel_registry::{PANELS, open_panel, panel_description, panel_for_slash_command};
use crate::kit::slash_completion::{SlashActionKind, SlashCompletion, SlashCompletionItem};
use crate::kit::submit_request::{SessionControlRequest, SubmitRequest, parse_submit_request};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

/// §10 queued 队列在 composer 上方最多显示的行数，超出显示 `· · ·`。
const QUEUE_VISIBLE_MAX: usize = 5;

/// [S2 单一事实源] 输入内容变化 → 焦点回到输入态：同步清除消息区 entry
/// 导航焦点（消息区仲裁与渲染同读 FOCUSED_ENTRY，无需 effect 收敛）。
///
/// [Why] 鼠标点击 chat entry 展开后 entry 导航焦点激活（FOCUSED_ENTRY =
/// Some），此时直接键入，Enter 仍被消息区消费为折叠切换/option 提交，输入框
/// 无法提交。鼠标点击输入框已在 Down handler 清除（见下方鼠标分支）；本函数
/// 覆盖键盘路径——所有修改 buffer 内容的按键/粘贴在写 state 前调用。
fn exit_entry_focus_on_edit() {
    if FOCUSED_ENTRY.state().read().is_some() {
        *FOCUSED_ENTRY.state().write() = None;
    }
}

/// 在 post_component_draw 时修复 CJK 续接 cell 的 diff 不可见性。
///
/// ratatui `set_stringn` 对双宽字符的续接 cell 始终 reset 到 `Cell::EMPTY`
/// (bg=Color::Reset, 无 modifier)。两帧续接 cell 相同 → diff 跳过 → 终端保留
/// 主 cell bg 的视觉扩展（光标白色残影）。
///
/// 此 hook 在每帧渲染后将续接 cell 标记 `AlwaysUpdate`，强制 diff 发送 SGR，
/// 但 **不修改 bg/fg 值**——视觉上完全透明，无底色。
struct CjkGhostFix;

impl Hook for CjkGhostFix {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        use ratatui::buffer::CellDiffOption;
        let area = drawer.area;
        let buf = drawer.buffer_mut();
        let right = area.right();
        let bottom = area.bottom();
        for y in area.y..bottom {
            let mut x = area.x;
            while x < right {
                let w = {
                    let symbol = buf[(x, y)].symbol();
                    if symbol.is_empty() {
                        0
                    } else {
                        symbol.chars().next().and_then(|c| c.width()).unwrap_or(0) as u16
                    }
                };
                if w > 1 {
                    for dx in 1..w {
                        let cx = x + dx;
                        if cx < right {
                            buf[(cx, y)].diff_option = CellDiffOption::AlwaysUpdate;
                        }
                    }
                    x += w;
                } else {
                    x += 1;
                }
            }
        }
    }
}

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

#[derive(Default, Props)]
pub struct InputAreaProps {
    pub loading: bool,
    pub hidden: bool,
    /// §11 高度降级：composer 编辑行数上限（`None` = 默认 10）。
    /// h<8 时由 `layout_plan` 传 `Some(2)`，钳制 TextArea 行数。
    pub max_lines: Option<u16>,
    /// §11 高度降级：session title（composer 上边栏）是否可见。
    /// h<12 时由 `layout_plan` 传 false 隐藏。
    pub session_title_visible: bool,
    /// [Fix §11] 输入区高度预算上限（SessionColumn = term_h - status - 3）：
    /// queued 队列/弹出层超过预算时优先截断队列，保证 transcript ≥3 行。
    /// `None` = 不限制（默认）。
    pub max_total_height: Option<u16>,
    /// §3.1/§10 水平网格（SessionColumn 传入）——composer prompt 前缀按
    /// gap 对齐 transcript content 起点；标题/footer 行按断点降级。
    pub grid: GridSpec,
}

#[component]
pub fn InputArea(props: &InputAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 单一编辑状态——闭包编辑 + 渲染读取共享同一实例
    let state = hooks.use_state(TextAreaState::default);
    // 终端窗口焦点：FocusGained/FocusLost 事件驱动，切换 tmux 窗格/终端标签时隐藏光标
    let term_focused = hooks.use_state(|| true);
    // i18n 语言切换订阅
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 追踪 composer 区域 + overlay 高度，用于鼠标点击→光标定位
    // area_tracker: 值拷贝模式（仿 MsgAreaTracker），避免每帧 Arc 重建导致 handler 读到 None
    let composer_area;
    {
        let tracker = hooks.use_hook(|| AreaTracker { rect: None });
        composer_area = tracker.rect; // 每帧取副本，区块结束即释放 &mut hooks 借用
    }
    let overlay_height = Arc::new(parking_lot::Mutex::new(0u16));

    // CJK 光标残影修复：post_component_draw 时标记续接 cell AlwaysUpdate
    hooks.use_hook(|| CjkGhostFix);

    // 终端焦点切换（tmux 窗格 / 终端标签切换）：FocusGained/FocusLost 更新 term_focused
    {
        let tf = term_focused;
        hooks.use_event_handler(
            ratatui_kit::prelude::EventScope::Global,
            ratatui_kit::prelude::EventPriority::Normal,
            move |event| match event {
                Event::FocusGained => {
                    *tf.write() = true;
                    EventResult::Consumed
                }
                Event::FocusLost => {
                    *tf.write() = false;
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            },
        );
    }

    // [Slice 3a] §3.1/§10 对齐：光标上下视觉移动的宽度 = 区域宽 - 正文起点
    // （prompt 前缀 + 右预留），随 grid gap 变化。
    let grid_for_visual = props.grid;
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
                        exit_entry_focus_on_edit();
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
                        exit_entry_focus_on_edit();
                        state.write().insert_char('\n');
                        EventResult::Consumed
                    }

                    // ── 编辑快捷键 ──
                    KeyCode::Char('w') if is_ctrl => {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        state.write().delete_word_backward();
                        EventResult::Consumed
                    }
                    KeyCode::Char('u') if is_ctrl => {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
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
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let mut s = state.write();
                        s.backspace();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('d') if is_ctrl && !mention_active && !slash_active => {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let mut s = state.write();
                        s.delete_forward();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('z')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let mut s = state.write();
                        s.undo();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('r')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let mut s = state.write();
                        s.redo();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('y')
                        if is_ctrl && !is_alt && !mention_active && !slash_active =>
                    {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
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
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
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
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
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
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
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
                        let tw = composer_area
                            .map(|a| {
                                a.width
                                    .saturating_sub(prompt_and_border_width(grid_for_visual))
                                    .max(1) as usize
                            })
                            .unwrap_or(80);
                        let moved = state.write().cursor_visual_up(tw);
                        if !moved {
                            let current = state.read().all_text();
                            if let Some(historical) = history_up(Some(&current)) {
                                exit_entry_focus_on_edit();
                                state.write().replace_all_no_undo(historical);
                            }
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed down");
                        let tw = composer_area
                            .map(|a| {
                                a.width
                                    .saturating_sub(prompt_and_border_width(grid_for_visual))
                                    .max(1) as usize
                            })
                            .unwrap_or(80);
                        let moved = state.write().cursor_visual_down(tw);
                        if !moved && let Some(historical) = history_down() {
                            exit_entry_focus_on_edit();
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
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let mut s = state.write();
                        s.insert_char(ch);
                        update_popup_prefix(&s);
                        *PREDICTION.state().write() = PredictionState::default();
                        EventResult::Consumed
                    }

                    // ── Ctrl+V 粘贴剪贴板（M6）──
                    // 在独立线程读 arboard（阻塞系统 I/O 不卡 UI），通过 state clone 回写 editor。
                    // 粘贴不应触发 slash/mention 弹窗（与 Event::Paste 分支一致）。
                    KeyCode::Char('v')
                        if is_ctrl && !is_alt && !is_shift && !mention_active && !slash_active =>
                    {
                        exit_history_mode_if_active();
                        exit_entry_focus_on_edit();
                        let state_clone = state;
                        std::thread::spawn(move || {
                            let Ok(mut cb) = arboard::Clipboard::new() else {
                                return;
                            };
                            // ── 图片粘贴分支 ──
                            // arboard 的 get_image() 需要新的 Clipboard 实例（之前的 cb 可能已被消费）
                            if let Some(arboard::ImageData {
                                bytes: image_bytes,
                                width,
                                height,
                            }) = arboard::Clipboard::new()
                                .ok()
                                .and_then(|mut cb2| cb2.get_image().ok())
                            {
                                let img_bytes = image_bytes.to_vec();
                                if !img_bytes.is_empty() {
                                    use std::hash::{DefaultHasher, Hash, Hasher};
                                    let mut hasher = DefaultHasher::new();
                                    img_bytes.hash(&mut hasher);
                                    let hash = format!("{:016x}", hasher.finish());
                                    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

                                    let img_dir = dirs_next::home_dir()
                                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                                        .join(".peri")
                                        .join("images");
                                    let _ = std::fs::create_dir_all(&img_dir);

                                    let file_name = format!("{}_{}.png", timestamp, &hash[..8]);
                                    let file_path = img_dir.join(&file_name);

                                    match png_encode(&img_bytes, width, height, &file_path) {
                                        Ok(()) => {
                                            let at_text = format!("@image {}", file_path.display());
                                            state_clone.write().insert_str(&at_text);
                                            return;
                                        }
                                        Err(_) => {
                                            // PNG 编码失败，静默回退到文本粘贴
                                        }
                                    }
                                }
                            }

                            // ── 文本粘贴分支（原有逻辑）──
                            let Ok(text) = cb.get_text() else {
                                return;
                            };
                            if text.is_empty() {
                                return;
                            }
                            const MAX: usize = 10_000;
                            let total = text.chars().count();
                            if total > MAX {
                                *crate::kit::atoms::NOTIFICATION.state().write() =
                                    Some(crate::kit::atoms::Notification {
                                        message: i18n::tr_args(
                                            "paste-truncated",
                                            &[("max".into(), FluentValue::from(MAX as i64))],
                                        ),
                                        until: std::time::Instant::now()
                                            + std::time::Duration::from_secs(2),
                                    });
                                let trunc: String = text.chars().take(MAX).collect();
                                state_clone.write().insert_str(&trunc);
                            } else {
                                state_clone.write().insert_str(&text);
                            }
                        });
                        *PREDICTION.state().write() = PredictionState::default();
                        EventResult::Consumed
                    }

                    // ── 预测文本接受（Tab）──
                    KeyCode::Tab => {
                        let pred = PREDICTION.state();
                        if !pred.read().text.is_empty() {
                            exit_entry_focus_on_edit();
                            let text = pred.read().text.clone();
                            *pred.write() = PredictionState::default();
                            exit_history_mode_if_active();
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
                exit_entry_focus_on_edit();
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
        let state_cl = state;
        let overlay_height_cl = overlay_height.clone();
        // [Slice 3a] §3.1/§10 对齐：正文起点 = prompt 前缀宽度（outer1+accent1+gap），
        // 随 grid gap 变化（gap=1 → 3，gap=2 → 4）。
        let grid_cl = props.grid;
        hooks.use_event_handler(
            ratatui_kit::prelude::EventScope::Global,
            ratatui_kit::prelude::EventPriority::High,
            move |event| {
                if let Event::Mouse(mouse) = event {
                    // 弹窗或面板激活时不处理鼠标——放行给前景 handler（如模型快速切换弹窗
                    // 锚定在状态栏上方、覆盖输入区时，点击弹窗行必须由弹窗消费）。
                    if mouse_router::is_occluded() {
                        return EventResult::Ignored;
                    }
                    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                        return EventResult::Ignored;
                    }
                    if let Some(outer) = composer_area {
                        let ov_h = *overlay_height_cl.lock();
                        let composer_top = outer.y.saturating_add(ov_h).saturating_add(1);
                        let text_x = outer.x.saturating_add(2 + grid_cl.gap);
                        // [FIX] 必须加上界：composer 下方是状态栏/通知行，行号同样 >= composer_top。
                        // 长文本（wrap > 10 行，editor_rows clamp 到 10）时 composer_height 不再
                        // 随文本增长，click_visual_row 可能落入 total_visual_rows 范围 → 误把
                        // 状态栏点击当作 composer 点击消费，status_bar 模型切换弹窗收不到事件。
                        if mouse.row >= composer_top
                            && mouse.row < outer.y.saturating_add(outer.height)
                            && mouse.column >= text_x
                        {
                            // [S2 单一事实源] 点击输入框 = 焦点回到输入态：事件
                            // 边界同步清除消息区 entry 导航焦点（消息区仲裁与
                            // 渲染同读 FOCUSED_ENTRY，无需 effect 收敛）——
                            // 否则点击展开后 Enter 仍被消息区消费为折叠切换，
                            // 输入框无法提交。
                            // [已知限制] Down handler 闭包不可注入测试（ratatui-kit
                            // dispatch pub(crate)）；本行迁移正确性由全库 grep 旧
                            // atom 名零残留 + focus_router_test 的
                            // focused=false → Enter 放行语义覆盖（S3 review M1）。
                            *FOCUSED_ENTRY.state().write() = None;
                            let click_visual_row = mouse.row.saturating_sub(composer_top) as usize;
                            let click_display_col = mouse.column.saturating_sub(text_x) as usize;
                            let s = state_cl.read();
                            if !s.text.is_empty() {
                                let tw = outer
                                    .width
                                    .saturating_sub(prompt_and_border_width(grid_cl))
                                    .max(1) as usize;
                                let wr = wrap_text(&s.text, s.cursor, tw);
                                if click_visual_row < wr.total_visual_rows {
                                    let vl = &wr.visual_lines[click_visual_row];
                                    let mut col = 0usize;
                                    let mut target_char = vl.char_range.0;
                                    for (i, ch) in vl.text.char_indices() {
                                        let cw =
                                            unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                                        if col + cw > click_display_col {
                                            break;
                                        }
                                        col += cw;
                                        target_char = vl.char_range.0
                                            + vl.text[..i + ch.len_utf8()].chars().count();
                                    }
                                    drop(s);
                                    state_cl.write().desired_col = None;
                                    state_cl.write().cursor = target_char;
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
    // 光标显示逻辑：loading 态始终显示；无面板/弹窗激活时显示；否则隐藏
    // use_atom 确保面板/弹窗变化时触发重渲染；*解引用取最新值
    let _panel_guard = hooks.use_atom(&ACTIVE_PANEL);
    let _popup_guard = hooks.use_atom(&POPUP_KIND);
    let active_panel = *ACTIVE_PANEL.state().read();
    let popup_kind = *POPUP_KIND.state().read();
    let show_cursor = *term_focused.read() && active_panel.is_none() && popup_kind.is_none();

    // 取消回滚文本恢复：use_effect 在 render 后运行，避免 render body 中写状态。
    // TurnInterrupted 递增 RENDER_HEARTBEAT → AppShell 重渲染 → InputArea 级联重渲染 → effect 执行。
    {
        let _hb = hooks.use_atom(&crate::kit::atoms::RENDER_HEARTBEAT);
        let hb_val = *_hb.read();
        let state_for_effect = state;
        hooks.use_effect(
            move || {
                if let Some(text) = crate::kit::atoms::INPUT_RESTORE_TEXT
                    .get()
                    .and_then(|mu| mu.try_lock())
                    .and_then(|mut g| g.take())
                {
                    state_for_effect.write().replace_all_no_undo(text);
                }
            },
            hb_val,
        );
    }

    // 选区范围（从 TextAreaState 传递到渲染器）
    let selection_range = editor.selection_range();
    // 占位符文本：优先使用 prediction（Tab 补全），其次使用 editor 设定的占位符
    let pred_text = hooks.use_atom(&PREDICTION).read().text.clone();
    let placeholder_str: Option<&str> = if !pred_text.is_empty() {
        Some(pred_text.as_str())
    } else if !editor.placeholder.is_empty() {
        Some(editor.placeholder.as_str())
    } else {
        None
    };

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

    // 计算 editor 视口高度（用于渲染窗口裁剪）
    // [Slice 3a] §3.1/§10 对齐：prompt 前缀宽度随 grid gap 变化
    // （outer1 + accent1 + gap），正文起点与 transcript content 起点一致。
    let text_width = composer_area
        .map(|a| {
            a.width
                .saturating_sub(prompt_and_border_width(props.grid))
                .max(1) as usize
        })
        .unwrap_or(80);
    let wrap = crate::components::textarea::wrap_text(&text, cursor, text_width);
    // [Slice 1c] §11 高度降级：h<8 时钳制编辑行数上限（max_lines），
    // 默认上限 10 保持不变。
    let max_editor_rows = props.max_lines.unwrap_or(10);
    let editor_rows = (wrap.total_visual_rows as u16).clamp(1, max_editor_rows);

    // 多行渲染——按 \n 拆分，每行作为独立 Line，光标高亮放在对应行
    // viewport_height 传入实际显示行数，render 内部只渲染该窗口大小的行
    let lines = render_multiline_with_cursor_for_themed(
        &text,
        cursor,
        selection_range,
        placeholder_str,
        text_width,
        editor_rows as usize,
        loading,
        show_cursor,
    );

    // 计算 composer 本体高度；popup 额外占位，避免被输入区自身裁切。
    let composer_height = editor_rows + 2;

    // 只在 slash 激活时克隆整个 item 列表——非激活态跳过 50+ item × 3 String 的堆分配
    let slash_items = if slash_active {
        get_cached_slash_items()
    } else {
        Vec::new()
    };
    let mention_select_state = state;
    let slash_select_state = state;

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

    // §10 queued（Slice 3 D4 反转）：loading 期间提交的 prompt 只入队
    // INPUT_BUFFER，渲染在 composer 上方（最多 5 条 + `· · ·`），不提前进
    // transcript；TurnDone/取消复位时 drain（本地气泡恰出现一次）。
    // [Fix §11] 输入区高度预算（`max_total_height` = term_h - status - 3）
    // 不足时**队列最先让位**——保证 transcript ≥3 行（40×8 + 排队场景不再
    // 把 transcript 挤到 2 行）；剩余排队项在 drain 时仍会发送，只是不可见。
    let input_buffer_handle = hooks.use_atom(&INPUT_BUFFER);
    let queue_items: Vec<String> = input_buffer_handle
        .read()
        .iter()
        .take(QUEUE_VISIBLE_MAX)
        .cloned()
        .collect();
    let queue_has_more = input_buffer_handle.read().len() > QUEUE_VISIBLE_MAX;
    let queue_lines = build_queue_lines(&queue_items, queue_has_more, text_width);
    let queue_height = if hidden || queue_lines.is_empty() {
        0
    } else {
        let n = queue_lines.len() as u16;
        match props.max_total_height {
            // 预算先保 composer + 弹出层，余量给队列；预算低于两者时队列隐藏。
            Some(budget) => n.min(budget.saturating_sub(composer_height + ov_height)),
            None => n,
        }
    };

    let total_height = if hidden {
        0
    } else {
        composer_height + ov_height + queue_height
    };

    let composer_lines = build_composer_lines(lines, loading, props.grid);

    // §10 composer 标题/footer（Slice 3a）：
    // - title_top 右侧 session title；
    // - title_bottom 左侧 `@ N files`（PENDING_ATTACHMENTS），右侧资源线
    //   （CPU% · MEM · ctx，原状态栏 Row1 第 4/5/7 项迁移）。
    // 窄屏逐级隐藏：h<12（session_title_visible=false）隐藏 title_top 整行；
    // h<8（max_lines=Some(2)）再隐藏 title_bottom。
    let ctx_usage = hooks.use_atom(&CONTEXT_USAGE);
    let attachments_handle = hooks.use_atom(&PENDING_ATTACHMENTS);
    let files_label = {
        let n = attachments_handle.read().len();
        (n > 0).then(|| {
            i18n::tr_args(
                "composer-attachments",
                &[("count".to_string(), FluentValue::from(n as u64))],
            )
        })
    };
    // 右侧资源线：CPU%（>50 显示）→ MEM（恒显）→ ctx，保持原状态栏顺序；
    // 全部 muted，不启用资源阈值色（与 composer footer 其余文本同色系）。
    let sem = THEME_ATOM.state().read().semantic;
    let footer_right: Option<Line<'static>> = {
        let snap = hooks.use_atom(&SERVICE_SNAPSHOT).read().clone();
        let mut spans: Vec<Span<'static>> = Vec::new();
        if snap.cpu_percent > 50.0 {
            spans.push(Span::styled(
                format!("CPU {:.0}%", snap.cpu_percent),
                Style::default().fg(sem.text.muted),
            ));
        }
        if !spans.is_empty() {
            spans.push(footer_separator(sem.text.muted));
        }
        spans.push(Span::styled(
            format!("MEM {}MB", snap.memory_mb),
            Style::default().fg(sem.text.muted),
        ));
        if let Some((pct, _)) = ctx_usage.read().as_ref() {
            spans.push(footer_separator(sem.text.muted));
            let c = i18n::tr_args(
                "composer-context-usage",
                &[("pct".to_string(), FluentValue::from(pct.round() as u64))],
            );
            spans.push(Span::styled(
                format!(" {c} "),
                Style::default().fg(sem.text.muted),
            ));
        }
        (!spans.is_empty()).then(|| Line::from(spans))
    };
    // 当前会话标题：service_snapshot 周期性派生；空标题或 §11 高度降级
    // （h<12，session_title_visible=false）时上边栏不渲染标签。
    let session_title = hooks.use_atom(&CURRENT_SESSION_TITLE).read().clone();
    let shown_session_title = if props.session_title_visible {
        session_title.as_str()
    } else {
        ""
    };

    // 显式背景色：防止 Paragraph 文本缩短时旧内容残留（ghosting）。
    // 未设背景时 ratatui 仅渲染文本 span，超出新文本的列保留终端原有像素。
    let composer_paragraph = Paragraph::new(composer_lines)
        .block(build_composer_block(
            loading,
            shown_session_title,
            files_label.as_deref(),
            footer_right,
            props.session_title_visible,
            props.max_lines.is_none(),
            composer_area.map(|a| a.width).unwrap_or(80),
        ))
        .style(Style::default().bg(THEME_ATOM.state().read().semantic.surface.default));

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
                            SlashActionKind::Command
                            | SlashActionKind::Skill
                            | SlashActionKind::McpSkill => {
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
            { if !hidden && !queue_lines.is_empty() {
                // §10 queued 队列（Slice 3b）：composer 边框上方的排队提示行，
                // 不进 transcript/不参与滚动模型；drain 后本列表随 buffer 清空。
                element!(
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Length(queue_height),
                    ) {
                        Text(text: Paragraph::new(queue_lines))
                    }
                ).into_any()
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
        }
    )
}

fn input_tokens() -> peri_theme::component::InputTokens {
    THEME_ATOM.state().read().component.input
}

/// §10 composer 边框：title_top 右侧 session title；title_bottom 左侧
/// `@ N files` + 右侧资源线（`footer_right`：CPU% · MEM · ctx，原状态栏迁移）。
/// 窄屏（§11）逐级隐藏：`show_top=false`（h<12）隐藏 title_top 整行；
/// `show_bottom=false`（h<8）隐藏 title_bottom。
/// `max_width` = composer 区域宽度（`use_previous_size`，resize 后次帧收敛）。
#[allow(clippy::too_many_arguments)] // 标题位/可见性/宽度参数同属一个边框语义，拆分反增复杂度
fn build_composer_block(
    loading: bool,
    session_title: &str,
    files_label: Option<&str>,
    footer_right: Option<Line<'static>>,
    show_top: bool,
    show_bottom: bool,
    max_width: u16,
) -> Block<'static> {
    let tokens = input_tokens();
    let sem = THEME_ATOM.state().read().semantic;
    let border_color = if loading {
        tokens.border_loading
    } else {
        tokens.border
    };

    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    if show_top && !session_title.is_empty() {
        let title_width = session_title.width().min(32) + 2;
        if title_width <= usize::from(max_width) {
            block = block.title_top(build_session_title_line(session_title).right_aligned());
        }
    }
    if show_bottom {
        // 左侧附件计数 / 右侧资源线（CPU·MEM·ctx，muted + 资源阈值色）
        if let Some(f) = files_label {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {f} "),
                Style::default().fg(sem.text.muted),
            )));
        }
        if let Some(line) = footer_right {
            block = block.title_bottom(line.right_aligned());
        }
    }
    block
}

/// 资源线分隔符：` · `（muted，与 composer footer 其余文本同色系）。
fn footer_separator(color: Color) -> Span<'static> {
    Span::styled(" · ", Style::default().fg(color))
}

/// §10 queued 队列行：`· {text}`（queued 符号 + muted），每行按 composer
/// 文本宽度截断；超过 [`QUEUE_VISIBLE_MAX`] 条时末行 `· · ·`。
fn build_queue_lines(items: &[String], has_more: bool, max_width: usize) -> Vec<Line<'static>> {
    if items.is_empty() {
        return Vec::new();
    }
    let sem = THEME_ATOM.state().read().semantic;
    let muted = Style::default().fg(sem.text.muted);
    let sym = crate::kit::terminal_caps::symbols(&crate::kit::atoms::TERMINAL_CAPS.state().read());
    let mut lines = Vec::with_capacity(items.len() + usize::from(has_more));
    for text in items {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", sym.queued), muted),
            Span::styled(crate::truncate::truncate_by_width(text, max_width), muted),
        ]));
    }
    if has_more {
        lines.push(Line::from(vec![Span::styled(
            format!("{} {} {}", sym.queued, sym.queued, sym.queued),
            muted,
        )]));
    }
    lines
}

/// §10 对齐：composer 正文起点 = prompt 前缀宽度（outer1 + accent1 + gap，
/// 与 transcript `first_prefix_width` 一致）+ 右预留 2 列。gap=1 → 5；
/// gap=2 → 6。
fn prompt_and_border_width(grid: GridSpec) -> u16 {
    (2 + grid.gap) + 2
}

/// 会话标题标签：hash 稳定底色 + 按亮度反色前景 + BOLD。
///
/// 同一标题经确定性 hash 后始终命中同一底色，不同标题大概率不同色；
/// 底色来自主题 `input.session_title_palette`，遵循"主题不硬编码颜色"约束。
fn build_session_title_line(title: &str) -> Line<'static> {
    let palette = input_tokens().session_title_palette;
    let bg = palette[stable_hash(title) as usize % palette.len()];
    Line::from(Span::styled(
        format!(" {} ", truncate_title_to_width(title, 32)),
        Style::default()
            .bg(bg)
            .fg(readable_fg(bg))
            .add_modifier(Modifier::BOLD),
    ))
}

/// FNV-1a 64 位确定性 hash——不依赖 `std` DefaultHasher 的随机 seed，
/// 保证同一标题在跨进程 / 跨会话场景下颜色稳定。
fn stable_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 按终端显示宽度截断标题（CJK 双宽字符按 2 列计），超长补省略号。
///
/// 委托共享 helper `crate::truncate::truncate_by_width`（历史实现已迁移，语义不变）。
fn truncate_title_to_width(s: &str, max_width: usize) -> String {
    crate::truncate::truncate_by_width(s, max_width)
}

/// 根据底色亮度选择黑白对比前景（保证可读性的"反色"效果）。
fn readable_fg(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
            if luminance > 140.0 {
                Color::Black
            } else {
                Color::White
            }
        }
        _ => Color::White,
    }
}

/// §10/§3.1 对齐：prompt 前缀宽度 = outer(1) + accent(1) + gap ——与 transcript
/// content 起点（`first_prefix_width`）一致。composer 无左右 border
/// （`Borders::TOP|BOTTOM`），正文起点即前缀宽度：gap=1 → `" ❯ "`（3 列），
/// gap=2 → `" ❯  "`（4 列）。续行前缀同宽（accent 位置留空）。
fn build_composer_lines(
    editor_lines: Vec<Line<'static>>,
    loading: bool,
    grid: GridSpec,
) -> Vec<Line<'static>> {
    let tokens = input_tokens();
    let mut lines = Vec::with_capacity(editor_lines.len().max(1));
    let prompt_style = Style::default()
        .fg(if loading {
            tokens.prompt_loading
        } else {
            tokens.prompt
        })
        .add_modifier(Modifier::BOLD);

    let prompt_prefix = format!(" \u{276f}{}", " ".repeat(grid.gap as usize));
    let cont_prefix = " ".repeat(grid.first_prefix_width());

    if editor_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(prompt_prefix, prompt_style),
            Span::raw(""),
        ]));
        return lines;
    }

    for (index, line) in editor_lines.into_iter().enumerate() {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        if index == 0 {
            spans.push(Span::styled(prompt_prefix.clone(), prompt_style));
        } else {
            spans.push(Span::styled(
                cont_prefix.clone(),
                Style::default().fg(tokens.continuation),
            ));
        }
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

fn popup_height(item_count: usize) -> u16 {
    (item_count.max(1) as u16 + 2).min(THEME_ATOM.state().read().component.popup.inline_height)
}

fn submit_text(submitted: String) {
    let Some(request) = parse_submit_request(&submitted) else {
        return;
    };

    let is_loading = ACP_STATE.state().read().is_loading;
    dispatch_submit_request(request, is_loading, |request| {
        if let Some(tx) = SUBMIT_TX.get() {
            let _ = tx.send(request);
        }
    });
}

fn dispatch_submit_request<F>(request: SubmitRequest, is_loading: bool, mut send_request: F)
where
    F: FnMut(SubmitRequest),
{
    match request {
        SubmitRequest::OpenPanel(kind) => open_panel(kind),
        SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup) => {
            *WIZARD_ACTIVE.state().write() = true;
        }
        SubmitRequest::AgentText(text) => {
            push_history(&text);
            reset_history_cursor();
            if is_loading {
                // §10 queued（Slice 3 D4 反转）：loading 期间**只入队**，不提前进
                // transcript——排队项显示在 composer 上方队列；TurnDone/取消
                // 复位时 drain（send_local_user_bubble + AgentText），气泡恰出现
                // 一次。保留 32 条上限（防无限堆积）。
                let input_buffer = INPUT_BUFFER.state();
                let mut guard = input_buffer.write();
                guard.push_back(text);
                while guard.len() > 32 {
                    guard.pop_front();
                }
            } else {
                // 通过 LOCAL_EVENT_TX 发送 LocalUserBubble 事件到 acp_bridge，
                // 统一走 dispatch_and_notify → push_view_models 写入路径。
                send_local_user_bubble(&text);
                send_request(SubmitRequest::AgentText(text));
            }
        }
        request @ (SubmitRequest::SessionControl(_)
        | SubmitRequest::ViewAction(_)
        | SubmitRequest::KeepGoing) => {
            if is_loading {
                show_submit_blocked_notification(&request);
            } else {
                send_request(request);
            }
        }
    }
}

fn show_submit_blocked_notification(request: &SubmitRequest) {
    let message = match request {
        SubmitRequest::SessionControl(_) => i18n::tr("submit-blocked"),
        SubmitRequest::ViewAction(_) => i18n::tr("submit-blocked"),
        _ => return,
    };
    *crate::kit::atoms::NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message,
        until: std::time::Instant::now() + std::time::Duration::from_secs(3),
    });
    crate::kit::atoms::RENDER_HEARTBEAT
        .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
}

/// 发送本地 user bubble 事件（`LocalUserBubble`）到 acp_bridge。
///
/// pub(crate)：非 loading 提交路径与 `acp_events::render::drain_input_buffer`
/// （Slice 3 D4）共用——drain 排队项时镜像非 loading 路径，先本地气泡再提交。
pub(crate) fn send_local_user_bubble(text: &str) {
    use crate::kit::acp_types::AcpEventData;
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            event: AcpEventData::LocalUserBubble {
                text: text.to_string(),
            },
            active_session_id: String::new(),
        });
    }
}

/// 退出 history 浏览模式（如果当前正在浏览）。
///
/// 任何改变编辑文本的 handler 都应在写入前调用：保留当前编辑内容作为新草稿，
/// 但清掉 `INPUT_HISTORY_INDEX` 指针，避免下一次 history_up 复用陈旧的浏览位置。
/// 非历史模式下调用为 no-op。
fn exit_history_mode_if_active() {
    use crate::kit::atoms::INPUT_HISTORY_INDEX;
    if INPUT_HISTORY_INDEX.state().read().is_some() {
        reset_history_cursor();
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
        let before = crate::components::textarea::History::snapshot(state);
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
    let skill_names: std::collections::HashSet<String> = SKILL_NAMES
        .state()
        .read()
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    let mcp_skill_names: std::collections::HashSet<String> = MCP_SKILL_NAMES
        .state()
        .read()
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    let mut items = Vec::with_capacity(PANELS.len() + remote.len() + 1);
    for panel in PANELS {
        if panel.slash_command.is_empty() {
            continue;
        }
        let slash_name = panel.slash_command.to_string();
        items.push(SlashCompletionItem {
            label_lowercase: slash_name.to_lowercase(),
            label: slash_name.clone(),
            insert_text: slash_name,
            description: panel_description(panel.kind),
            kind: SlashActionKind::Panel,
        });
    }
    // /setup 命令：打开 Setup Wizard 引导界面
    items.push(SlashCompletionItem {
        label: "setup".to_string(),
        insert_text: "setup".to_string(),
        description: i18n::tr("command-setup-description"),
        kind: SlashActionKind::Command,
        label_lowercase: "setup".to_string(),
    });
    for (name, description) in &remote {
        // S16：根据 SKILL_NAMES / MCP_SKILL_NAMES 区分 McpSkill / Skill / Command，
        // 优先级 McpSkill > Skill > Command（先判 MCP_SKILL_NAMES）。
        let name_lower = name.to_lowercase();
        let kind = if mcp_skill_names.contains(&name_lower) {
            SlashActionKind::McpSkill
        } else if skill_names.contains(&name_lower) {
            SlashActionKind::Skill
        } else {
            SlashActionKind::Command
        };
        items.push(SlashCompletionItem {
            label: name.clone(),
            insert_text: name.clone(),
            description: description.clone(),
            kind,
            label_lowercase: name_lower,
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

#[allow(clippy::too_many_arguments)]
fn render_multiline_with_cursor_for_themed(
    text: &str,
    cursor: usize,
    selection_range: Option<(usize, usize)>,
    placeholder: Option<&str>,
    max_width: usize,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<ratatui::text::Line<'static>> {
    let tokens = input_tokens();
    let cursor_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::BOLD);
    let selection_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::DIM);
    let placeholder_style = Style::default().fg(tokens.placeholder);
    let default_style = Style::default().bg(Color::Reset);
    crate::components::textarea::render_multiline_with_cursor(
        text,
        cursor,
        cursor_style,
        selection_range,
        selection_style,
        placeholder,
        placeholder_style,
        default_style,
        max_width,
        viewport_height,
        loading,
        show_cursor,
    )
}

/// 将 RGBA 字节数组编码为 PNG 文件
pub(crate) fn png_encode(
    rgba_bytes: &[u8],
    width: usize,
    height: usize,
    output_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(output_path)?;
    let mut w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut w, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba_bytes)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
#[path = "input_area_test.rs"]
mod tests;
