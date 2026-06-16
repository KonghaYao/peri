// ── Event module ──────────────────────────────────────────────────────────────
// Split from the original monolithic event.rs (1447 lines) into:
//   mouse.rs   — mouse coordinate helpers + clipboard functions
//   keyboard.rs — key event handler
//   macros.rs  — panel dispatch macros (with_global_panels!, with_session_panels!)
//   mod.rs     — Action, event loop, dispatcher, OAuth handling

pub mod keyboard;
mod macros;
pub mod mouse;

use std::cell::RefCell;
use std::time::Duration;
#[cfg(target_os = "windows")]
use std::time::Instant;

use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use tui_textarea::{Input, Key};

use crate::app::{
    panel_manager::{EventResult, PanelKind},
    App,
};
use crate::{with_global_panels, with_session_panels};

// ── Event stash ──────────────────────────────────────────────────────────────

// Thread-local event buffer for stashing non-scroll events encountered
// during `coalesce_mouse_events`. When coalescing hits a non-scroll event
// (keypress, click, etc.), the event is stashed here instead of replacing
// the scroll, so the scroll is processed first and the stashed event is
// returned on the next read.
thread_local! {
    static EVENT_STASH: RefCell<Option<Event>> = const { RefCell::new(None) };
}

// Timestamp of the most recent MouseScroll event seen by the filter.
// Used to detect orphaned wheel-generated Key(Up/Down) events that have
// no paired MouseScroll following them in the queue (the last Key in
// each interleaved batch).
#[cfg(target_os = "windows")]
thread_local! {
    static LAST_MOUSE_SCROLL_TIME: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

// ── Action ──────────────────────────────────────────────────────────────────

pub enum Action {
    Quit,
    Submit(String),
    Redraw,
}

// ── Event loop ──────────────────────────────────────────────────────────────

pub async fn next_event(app: &mut App) -> Result<Option<Action>> {
    // Quit-pending state auto-expires after 1s; trigger redraw so the shortcut bar
    // returns to normal
    if let Some(since) = app.global_ui.quit_pending_since {
        if since.elapsed() >= std::time::Duration::from_secs(1) {
            app.global_ui.quit_pending_since = None;
            return Ok(Some(Action::Redraw));
        }
    }

    // Rewind-pending state auto-expires after 2s
    if let Some(since) = app.global_ui.rewind_pending_since {
        if since.elapsed() >= std::time::Duration::from_secs(2) {
            app.global_ui.rewind_pending_since = None;
            return Ok(Some(Action::Redraw));
        }
    }

    // 关键修复：函数开头消费 stash。早期实现把 take 放在内层 loop 开头，
    // 晚于 poll(50ms) 超时返回 Ok(None) 路径——stash 有事件但队列为空时会
    // 反复超时返回，stash 一直挂着，导致后续用户按键被旧 stashed 事件
    // 抢先处理，主输入框光标位置与实际编辑位置不同步（按一次 ← 左移两次）。
    // UI 计时（quit/rewind）优先级高于事件，已在上面先于 stash take 处理。
    let stashed = EVENT_STASH.with(|s| s.borrow_mut().take());

    // Mouse-availability probe: on first user input after startup, determine
    // whether the terminal supports mouse events. probe 仅启动时跑一次，
    // stash 此时不可能有值；保留 stash 短路以防御性避免 stash 事件被探测路径吞掉。
    if app.global_ui.mouse_available.is_none() && stashed.is_none() {
        // Wait for the first event (up to 1 s); this is not counted as normal poll timeout
        if event::poll(Duration::from_secs(1))? {
            let ev = event::read()?;
            if matches!(ev, Event::Mouse(_)) {
                app.global_ui.mouse_available = Some(true);
            } else {
                // Received keyboard/resize etc. but not mouse → terminal likely
                // does not support mice (mouse-capable terminals almost always trigger
                // scroll/move within 1 s)
                app.global_ui.mouse_available = Some(false);
            }
            return handle_event(app, ev).await;
        } else {
            // No event within 1 s → no mouse
            app.global_ui.mouse_available = Some(false);
            return Ok(None);
        }
    }

    // stash 有事件时跳过 poll 直接用 stashed，避免 poll 超时返回 Ok(None)
    // 跳过 stash 消费。stash 为空时保持原 poll 超时行为。
    let ev = if let Some(stashed) = stashed {
        // stash 事件来源（coalesce_mouse_events 或 Windows filter）已过
        // 一次处理，不再重新走 Windows filter——filter 的 peek 路径在
        // stash 命中时队列状态不确定，可能引入不必要的延迟或丢失。
        stashed
    } else {
        if !event::poll(Duration::from_millis(50))? {
            return Ok(None);
        }
        // Windows: mouse wheel movement may generate spurious Key(Up/Down)
        // events alongside MouseScroll events. Filter these out before
        // coalescing so that scroll events are handled correctly. When the
        // filter discards a spurious Key, loop to read the next real event.
        #[cfg(target_os = "windows")]
        {
            loop {
                let ev = event::read()?;
                if let Some(ev) = filter_mouse_wheel_keys(ev) {
                    break ev;
                }
            }
        }
        // Non-Windows: no filter needed, read directly.
        #[cfg(not(target_os = "windows"))]
        event::read()?
    };

    // Scroll/Drag event coalescing: drain queued mouse events to avoid
    // redundant redraws during rapid scrolling or scrollbar dragging.
    let ev = coalesce_mouse_events(ev);

    // Simulated-paste detection: on terminals without bracketed paste support
    // (Windows), multi-line paste arrives as a rapid burst of key events.
    // Detect this pattern and convert to Event::Paste so the normal paste
    // handler inserts the full text into the textarea.
    let ev = detect_simulated_paste(ev);

    handle_event(app, ev).await
}

// ── Mouse-wheel key filter (Windows only) ─────────────────────────────────

/// On Windows Terminal (via ConPTY), a single mouse wheel tick can generate
/// both a `Key(Up/Down)` event AND `MouseScrollUp/Down` events. The key
/// event arrives interleaved with the scroll events in the input queue,
/// causing the textarea to scroll (via `handle_up`/`handle_down`) before
/// the messages area.
///
/// This function uses a two-phase strategy:
///
/// **Phase 1 — peek forward**: when a bare `Key(Up/Down)` is read, peek at
/// the next event. If a `MouseScroll` follows immediately in the queue, the
/// key is discarded and the mouse scroll is returned instead.
///
/// **Phase 2 — look backward**: when a bare `Key(Up/Down)` arrives without
/// a paired MouseScroll (orphaned last Key in an interleaved batch), and a
/// MouseScroll was recently processed (< 200 ms), the Key is discarded as
/// spurious (returns `None`). This prevents the orphaned Key from reaching
/// `handle_up`/`handle_down` and scrolling the textarea.
///
/// Trade-off: a genuine Key(Up/Down) press within 200 ms of a mouse scroll
/// will be discarded. In practice this is far less disruptive than the
/// textarea intercepting wheel scroll events.
#[cfg(target_os = "windows")]
fn filter_mouse_wheel_keys(ev: Event) -> Option<Event> {
    // Record MouseScroll timestamps for backward-looking checks (Phase 2).
    // A normal MouseScroll event passes through unchanged.
    if matches!(
        &ev,
        Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
    ) {
        LAST_MOUSE_SCROLL_TIME.with(|s| *s.borrow_mut() = Some(Instant::now()));
        return Some(ev);
    }

    let is_bare_updown = matches!(&ev,
        Event::Key(k) if matches!(k.code, KeyCode::Up | KeyCode::Down)
            && k.modifiers == KeyModifiers::NONE
            && k.kind == KeyEventKind::Press
    );
    if !is_bare_updown {
        return Some(ev);
    }

    // Phase 1: peek forward for an immediately following MouseScroll.
    // Two-stage peek to handle the common case where ConPTY delivers the
    // MouseScroll with a micro-delay after the Key(Up/Down).
    //
    // Stage 1a — instant peek: catch MouseScroll already in the queue.
    // Stage 1b — brief wait: give ConPTY time to buffer the MouseScroll
    //   when it hasn't arrived yet. The wait duration is adaptive:
    //   • 10 ms when this is the first scroll in a batch (no Phase 2
    //     backstop — a leaked Key would reach the textarea unconditionally).
    //   • 3 ms when a recent MouseScroll exists (Phase 2 backstop will
    //     catch any remaining leak, so the shorter wait suffices).
    //
    // Genuine arrow-key presses are unaffected: the first press may incur
    // a ~10 ms delay (imperceptible), and the poll returns false quickly.
    let has_recent_scroll = LAST_MOUSE_SCROLL_TIME.with(|s| {
        s.borrow()
            .map(|t| t.elapsed() < Duration::from_millis(200))
            .unwrap_or(false)
    });

    if event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(next) = event::read() {
            if matches!(&next,
                Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
            ) {
                LAST_MOUSE_SCROLL_TIME.with(|s| *s.borrow_mut() = Some(Instant::now()));
                return Some(next);
            }
            EVENT_STASH.with(|s| *s.borrow_mut() = Some(next));
        }
    } else {
        let wait_ms = if has_recent_scroll { 3 } else { 10 };
        if event::poll(Duration::from_millis(wait_ms)).unwrap_or(false) {
            if let Ok(next) = event::read() {
                if matches!(&next,
                    Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
                ) {
                    LAST_MOUSE_SCROLL_TIME.with(|s| *s.borrow_mut() = Some(Instant::now()));
                    return Some(next);
                }
                EVENT_STASH.with(|s| *s.borrow_mut() = Some(next));
            }
        }
    }

    // Phase 2: check backward — was a MouseScroll recently processed?
    if has_recent_scroll {
        // Orphaned wheel-generated Key detected. Discard it to prevent
        // textarea scrolling. Loop in next_event() reads the next event.
        return None;
    }

    Some(ev)
}

// ── Mouse event coalescing ───────────────────────────────────────────────

/// Coalesces rapid-fire mouse scroll/drag events from the crossterm queue.
///
/// When a Scroll or Drag(Left) mouse event is the initial event, drains any
/// additional queued events using a non-blocking poll, keeping only the last
/// coalesceable event. This trades scroll precision for CPU: N scroll events
/// within one poll cycle (~50ms) result in only ±3 lines moved instead of N×3.
/// Drag(Left) is unaffected since only the final position matters.
///
/// Non-coalesceable events (click, keypress, etc.) are stashed for the next
/// read via `EVENT_STASH` rather than replacing the current scroll event.
fn coalesce_mouse_events(ev: Event) -> Event {
    // Only activate coalescing for scroll and drag mouse events
    match &ev {
        Event::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::Drag(MouseButton::Left) => {}
            _ => return ev,
        },
        _ => return ev,
    }

    let mut last_ev = ev;

    // Drain all queued scroll/drag events, keeping only the last one.
    // Non-scroll/drag events are stashed for the next read so the scroll
    // event is not replaced.
    while event::poll(Duration::ZERO).unwrap_or(false) {
        let next = match event::read() {
            Ok(e) => e,
            Err(_) => break,
        };
        match &next {
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::Drag(MouseButton::Left) => {
                    last_ev = next;
                }
                // Other mouse events (click, release, move): stash, stop draining
                _ => {
                    EVENT_STASH.with(|s| *s.borrow_mut() = Some(next));
                    break;
                }
            },
            // Non-mouse events: stash, stop draining
            _ => {
                EVENT_STASH.with(|s| *s.borrow_mut() = Some(next));
                break;
            }
        }
    }

    last_ev
}

// ── Simulated-paste detection (Windows) ───────────────────────────────

/// On terminals that do not support bracketed paste (e.g. Windows cmd.exe,
/// legacy PowerShell), multi-line paste is simulated as a rapid burst of
/// individual Key events — each character becomes a Char event and each
/// newline becomes a bare Enter event.
///
/// This function detects that pattern: when a bare Enter arrives and the
/// event queue already contains more Key events buffered behind it (within
/// a 1 ms window), we drain the entire burst, reconstruct the pasted text,
/// and return an `Event::Paste` so the normal paste path handles it.
///
/// A 1 ms poll window is too short for human typing to trigger, so false
/// positives are negligible.
fn detect_simulated_paste(ev: Event) -> Event {
    match &ev {
        Event::Key(k)
            if k.code == KeyCode::Enter
                && k.modifiers == KeyModifiers::NONE
                && k.kind == KeyEventKind::Press => {}
        _ => return ev,
    }

    // Quick probe: any queued event within 1 ms?
    if !event::poll(Duration::from_millis(1)).unwrap_or(false) {
        return ev; // No queued events → manual Enter → submit normally
    }

    // Simulated paste detected. Collect the full burst into text.
    let mut text = String::from('\n');

    // Read the first queued event we already probed
    if let Ok(next) = event::read() {
        key_event_to_text(next, &mut text);
    }

    // Drain remaining queued events (ZERO = non-blocking)
    while event::poll(Duration::ZERO).unwrap_or(false) {
        match event::read() {
            Ok(next) => key_event_to_text(next, &mut text),
            Err(_) => break,
        }
    }

    // If the burst only contained the initial Enter's own Release event
    // (filtered out by key_event_to_text), this is a genuine Enter press,
    // not a simulated paste. Return the original event.
    if text == "\n" {
        return ev;
    }

    Event::Paste(text)
}

/// Append a single crossterm `Event` into `text` for simulated-paste
/// reconstruction. Key(Char) appends the character; Key(Enter) appends
/// `\n`; Key(Tab) appends `\t`; Key(Backspace) removes the last char;
/// everything else (modifiers, non-printable keys) terminates the drain.
fn key_event_to_text(ev: Event, text: &mut String) {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
            KeyCode::Char(c) => {
                // Ctrl+char or Alt+char during paste → stop collecting
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    || k.modifiers.contains(KeyModifiers::ALT)
                {
                    // Flush remaining: stop collecting but don't lose the event.
                    // Since we can't re-inject, treat modifier+char as literal.
                    text.push(c);
                } else {
                    text.push(c);
                }
            }
            KeyCode::Enter => text.push('\n'),
            KeyCode::Tab => text.push('\t'),
            KeyCode::Backspace => {
                text.pop();
            }
            _ => {} // Ignore other keys (arrows, etc.) during paste
        },
        Event::Mouse(_) | Event::FocusGained | Event::FocusLost | Event::Resize(_, _) => {
            // Non-key events shouldn't appear in a paste burst; stop collecting.
        }
        Event::Paste(p) => {
            // Rare: a real Paste event appeared mid-burst (shouldn't happen).
            text.push_str(&p);
        }
        _ => {}
    }
}

// ── Event dispatcher ────────────────────────────────────────────────────────

/// Core event-handling logic (extracted from `next_event` to avoid duplicating
/// the probe and normal paths).
async fn handle_event(app: &mut App, ev: Event) -> Result<Option<Action>> {
    match ev {
        Event::FocusGained => {
            app.focused = true;
            return Ok(Some(Action::Redraw));
        }
        Event::FocusLost => {
            app.focused = false;
            return Ok(Some(Action::Redraw));
        }
        Event::Resize(_, _) => {
            // Width sync is now driven by render_messages (compares cache.width vs text_area.width)
            app.session_mgr.current_mut().ui.text_selection.clear();
        }
        Event::Key(key_event) => {
            return keyboard::handle_key_event(app, key_event);
        }
        Event::Paste(text) => {
            // Paste text handling
            // Some terminals (e.g. VSCode) use \r instead of \n as line separator in bracketed paste
            let text = text.replace('\r', "\n");

            // Setup wizard open — paste into active field
            if let Some(wizard) = &mut app.global_ui.setup_wizard {
                wizard.paste_text(&text);
                return Ok(Some(Action::Redraw));
            }

            // ─── 交互弹窗优先路由（AskUser/HITL/OAuth） ──────────────────
            // 弹窗激活时，Paste（含终端 IME 组合后的中文）应进入弹窗
            // 而非 textarea。仅 AskUser 弹窗有 custom_input 接收文本。
            if app.is_interaction_popup_active() {
                app.paste_to_interaction_popup(&text);
                return Ok(Some(Action::Redraw));
            }

            // ─── PanelManager paste dispatch ────────────────────────────
            {
                // Session panels: Model, Agent, Hooks, Login, Config, ThreadBrowser
                let session_kind = app.session_mgr.current_mut().session_panels.active_kind();
                if matches!(
                    session_kind,
                    Some(PanelKind::Model)
                        | Some(PanelKind::Agent)
                        | Some(PanelKind::Hooks)
                        | Some(PanelKind::Login)
                        | Some(PanelKind::Config)
                        | Some(PanelKind::ThreadBrowser)
                ) {
                    with_session_panels!(app, |sp, ctx| sp.dispatch_paste(&text, &mut ctx));
                    return Ok(Some(Action::Redraw));
                }

                // Global panels: Status, Memory, Mcp, Cron, Plugin, Betas
                let global_kind = app.global_panels.active_kind();
                if matches!(
                    global_kind,
                    Some(PanelKind::Status)
                        | Some(PanelKind::Memory)
                        | Some(PanelKind::Mcp)
                        | Some(PanelKind::Cron)
                        | Some(PanelKind::Plugin)
                        | Some(PanelKind::Betas)
                ) {
                    with_global_panels!(app, |pm, ctx| pm.dispatch_paste(&text, &mut ctx));
                    return Ok(Some(Action::Redraw));
                }
            }

            // Fallback: paste into textarea
            // 弹窗激活时不写入 textarea——用户应通过弹窗 UI 交互
            if !app.is_interaction_popup_active() {
                app.session_mgr.current_mut().ui.textarea.insert_str(&text);
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            // ── AskUser 弹窗鼠标交互（优先于面板/消息区） ────────────────────────
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                {
                    if let Some(crate::app::InteractionPrompt::Questions(_)) =
                        app.session_mgr.current_mut().agent.interaction_prompt
                    {
                        if let Some(area) = app.session_mgr.current_mut().ui.panel_area {
                            if mouse::mouse_in_rect(&mouse, area) {
                                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                                    -3
                                } else {
                                    3
                                };
                                app.ask_user_scroll(delta);
                                return Ok(Some(Action::Redraw));
                            }
                        }
                    }
                }
                // 正常滚动处理
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let panel_area = app.session_mgr.current_mut().ui.panel_area;
                        if let Some(area) = panel_area {
                            if mouse::mouse_in_rect(&mouse, area) {
                                // Session panel takes priority
                                let sp = &app.session_mgr.current_mut().session_panels;
                                if sp.is_any_open() {
                                    let result = with_session_panels!(app, |sp, ctx| {
                                        sp.dispatch_scroll(-3, &mut ctx)
                                    });
                                    if result == EventResult::Consumed {
                                        return Ok(Some(Action::Redraw));
                                    }
                                }
                                // Global panel
                                if app.global_panels.is_any_open() {
                                    let result = with_global_panels!(app, |pm, ctx| {
                                        pm.dispatch_scroll(-3, &mut ctx)
                                    });
                                    if result == EventResult::Consumed {
                                        return Ok(Some(Action::Redraw));
                                    }
                                }
                            }
                        }
                        app.scroll_up();
                    }
                    MouseEventKind::ScrollDown => {
                        let panel_area = app.session_mgr.current_mut().ui.panel_area;
                        if let Some(area) = panel_area {
                            if mouse::mouse_in_rect(&mouse, area) {
                                // Session panel takes priority
                                let sp = &app.session_mgr.current_mut().session_panels;
                                if sp.is_any_open() {
                                    let result = with_session_panels!(app, |sp, ctx| {
                                        sp.dispatch_scroll(3, &mut ctx)
                                    });
                                    if result == EventResult::Consumed {
                                        return Ok(Some(Action::Redraw));
                                    }
                                }
                                // Global panel
                                if app.global_panels.is_any_open() {
                                    let result = with_global_panels!(app, |pm, ctx| {
                                        pm.dispatch_scroll(3, &mut ctx)
                                    });
                                    if result == EventResult::Consumed {
                                        return Ok(Some(Action::Redraw));
                                    }
                                }
                            }
                        }
                        app.scroll_down();
                    }
                    _ => unreachable!(),
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // ── AskUser 弹窗滚动条点击（优先于面板滚动条） ──────────────────
                {
                    if let Some(crate::app::InteractionPrompt::Questions(ref p)) =
                        app.session_mgr.current_mut().agent.interaction_prompt
                    {
                        if let Some(metrics) = p.scrollbar_metrics {
                            if mouse.column >= metrics.bar_area.x
                                && mouse.column < metrics.bar_area.x + metrics.bar_area.width
                                && mouse.row >= metrics.bar_area.y
                                && mouse.row < metrics.bar_area.bottom()
                                && metrics.max_offset > 0
                            {
                                let bar_inner_height = metrics.bar_area.height.saturating_sub(2);
                                if bar_inner_height > 0 {
                                    let rel_y = (mouse.row.saturating_sub(metrics.bar_area.y + 1))
                                        .min(bar_inner_height);
                                    let new_offset = ((rel_y as f64 / bar_inner_height as f64)
                                        * metrics.max_offset as f64)
                                        as u16;
                                    let new_offset = new_offset.min(metrics.max_offset);
                                    if let Some(crate::app::InteractionPrompt::Questions(p)) = app
                                        .session_mgr
                                        .current_mut()
                                        .agent
                                        .interaction_prompt
                                        .as_mut()
                                    {
                                        p.scroll_offset = new_offset;
                                    }
                                }
                                return Ok(Some(Action::Redraw));
                            }
                        }
                    }
                }
                // Panel scrollbar: ▲/▼ buttons and bar click/drag
                // Must be checked BEFORE dispatch_mouse so scrollbar clicks
                // aren't consumed by panel content area handlers.
                {
                    let session = &mut app.session_mgr.current_mut();
                    if let Some(ref metrics) = session.ui.panel_scrollbar_metrics {
                        // ▼ button click (scroll to bottom)
                        if let Some(btn) = metrics.down_btn_area {
                            if mouse.column >= btn.x
                                && mouse.column < btn.x + btn.width
                                && mouse.row >= btn.y
                                && mouse.row < btn.y + btn.height
                            {
                                session
                                    .session_panels
                                    .dispatch_set_scroll_offset(metrics.max_offset);
                                session.ui.panel_scroll_offset = metrics.max_offset;
                                return Ok(Some(Action::Redraw));
                            }
                        }
                        // ▲ button click (scroll to top)
                        if let Some(btn) = metrics.up_btn_area {
                            if mouse.column >= btn.x
                                && mouse.column < btn.x + btn.width
                                && mouse.row >= btn.y
                                && mouse.row < btn.y + btn.height
                            {
                                session.session_panels.dispatch_set_scroll_offset(0);
                                session.ui.panel_scroll_offset = 0;
                                return Ok(Some(Action::Redraw));
                            }
                        }
                        // Scrollbar bar click (proportional jump + start drag)
                        if mouse.column == metrics.bar_area.x
                            && mouse.row >= metrics.bar_area.y
                            && mouse.row < metrics.bar_area.bottom()
                            && metrics.max_offset > 0
                        {
                            let bar_inner_height = metrics.bar_area.height.saturating_sub(2);
                            if bar_inner_height > 0 {
                                let rel_y = (mouse.row.saturating_sub(metrics.bar_area.y + 1))
                                    .min(bar_inner_height);
                                let new_offset = ((rel_y as f64 / bar_inner_height as f64)
                                    * metrics.max_offset as f64)
                                    as u16;
                                let new_offset = new_offset.min(metrics.max_offset);
                                session
                                    .session_panels
                                    .dispatch_set_scroll_offset(new_offset);
                                session.ui.panel_scroll_offset = new_offset;
                                session.ui.panel_scrollbar_dragging = true;
                            }
                            return Ok(Some(Action::Redraw));
                        }
                    }
                }
                // Panel area: dispatch mouse click to panel content
                let panel_area = app.session_mgr.current_mut().ui.panel_area;
                let mut click_consumed = false;
                if let Some(area) = panel_area {
                    if mouse::mouse_in_rect(&mouse, area) {
                        // Session panels
                        {
                            let sp = &app.session_mgr.current_mut().session_panels;
                            if sp.is_any_open() {
                                let result = with_session_panels!(app, |sp, ctx| {
                                    sp.dispatch_mouse(mouse, area, &mut ctx)
                                });
                                if result == EventResult::Consumed {
                                    click_consumed = true;
                                }
                            }
                        }
                        // Global panels
                        if !click_consumed && app.global_panels.is_any_open() {
                            let result = with_global_panels!(app, |pm, ctx| {
                                pm.dispatch_mouse(mouse, area, &mut ctx)
                            });
                            if result == EventResult::Consumed {
                                click_consumed = true;
                            }
                        }
                    }
                }
                if click_consumed {
                    return Ok(Some(Action::Redraw));
                }
                // Panel area: start panel selection
                let panel_area = app.session_mgr.current_mut().ui.panel_area;
                if let Some(area) = panel_area {
                    if mouse::mouse_in_rect(&mouse, area) {
                        let content_row = mouse.row - area.y
                            + app.session_mgr.current_mut().ui.panel_scroll_offset;
                        let col = mouse.column - area.x;
                        app.session_mgr
                            .current_mut()
                            .ui
                            .panel_selection
                            .start_drag(content_row, col);
                        app.session_mgr.current_mut().ui.text_selection.clear();
                        // Don't process other-area selections
                        return Ok(Some(Action::Redraw));
                    }
                }
                if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                    let scroll_offset = app.session_mgr.current_mut().ui.scroll_offset;
                    let scroll_follow = app.session_mgr.current_mut().ui.scroll_follow;

                    // Scroll-to-bottom button: bottom-right click when user has scrolled away
                    let btn_col_start = area.right().saturating_sub(2);
                    let btn_row_start = area.bottom().saturating_sub(2);
                    if !scroll_follow
                        && mouse.column >= btn_col_start
                        && mouse.column < area.right()
                        && mouse.row >= btn_row_start
                        && mouse.row < area.bottom()
                    {
                        app.scroll_to_bottom();
                        return Ok(Some(Action::Redraw));
                    }

                    // Scroll-to-top button: top-right click when user has scrolled up
                    if scroll_offset > 0
                        && mouse.column >= btn_col_start
                        && mouse.column < area.right()
                        && mouse.row >= area.y
                        && mouse.row < area.y.saturating_add(2)
                    {
                        app.scroll_to_top();
                        return Ok(Some(Action::Redraw));
                    }

                    // Scrollbar drag: click on the rightmost scrollbar column
                    // (▲/▼ buttons already handled above, so this catches the track area)
                    // 记录起始位置，拖拽时用相对位移计算 offset，无需匹配 thumb 几何
                    let scrollbar_col = area.right().saturating_sub(1);
                    if mouse.column == scrollbar_col
                        && mouse.row >= area.y
                        && mouse.row < area.bottom()
                    {
                        let track = area.height;
                        if track > 0 {
                            let max_scroll = app.session_mgr.current_mut().ui.scrollbar_max_offset;
                            // 首次点击：按比例跳到点击位置
                            let rel_y = mouse.row.saturating_sub(area.y);
                            let new_offset = if max_scroll > 0 {
                                ((rel_y as f64 / track as f64) * max_scroll as f64)
                                    .clamp(0.0, max_scroll as f64)
                                    as u16
                            } else {
                                0
                            };
                            app.session_mgr.current_mut().ui.scroll_offset = new_offset;
                            app.session_mgr.current_mut().ui.scroll_follow = false;
                            app.session_mgr.current_mut().ui.scrollbar_dragging = true;
                            app.session_mgr.current_mut().ui.scrollbar_drag_start_y = mouse.row;
                            app.session_mgr.current_mut().ui.scrollbar_drag_start_offset =
                                new_offset;
                        }
                        return Ok(Some(Action::Redraw));
                    }

                    if mouse.row >= area.y
                        && mouse.row < area.y + area.height
                        && mouse.column >= area.x
                        && mouse.column < area.x + area.width
                    {
                        let visual_row =
                            mouse.row - area.y + app.session_mgr.current_mut().ui.scroll_offset;
                        let visual_col = mouse.column - area.x;
                        app.session_mgr
                            .current_mut()
                            .ui
                            .text_selection
                            .start_drag(visual_row, visual_col);
                    }
                }
                // Textarea area: start textarea selection
                // 弹窗激活时跳过——光标不应移到 textarea 内
                if !app.is_interaction_popup_active() {
                    if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                        if mouse.row >= area.y
                            && mouse.row < area.y + area.height
                            && mouse.column >= area.x
                            && mouse.column < area.x + area.width
                        {
                            let session = &app.session_mgr.current_mut();
                            let (row, col) =
                                mouse::textarea_mouse_to_cursor(&session.ui.textarea, area, &mouse);
                            app.session_mgr.current_mut().ui.textarea.move_cursor(
                                tui_textarea::CursorMove::Jump(row as u16, col as u16),
                            );
                            app.session_mgr.current_mut().ui.textarea.start_selection();
                        }
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Scrollbar drag: 用相对位移计算 offset，无需匹配 thumb 几何
                if app.session_mgr.current_mut().ui.scrollbar_dragging {
                    if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                        let track = area.height;
                        if track > 0 {
                            let max_scroll = app.session_mgr.current_mut().ui.scrollbar_max_offset;
                            let start_y = app.session_mgr.current_mut().ui.scrollbar_drag_start_y;
                            let start_offset =
                                app.session_mgr.current_mut().ui.scrollbar_drag_start_offset;
                            if max_scroll > 0 {
                                let delta_y = mouse.row as i32 - start_y as i32;
                                // 每移动 1 行 track = 移动 max_scroll / track 的 offset
                                let delta_offset =
                                    (delta_y as f64 * (max_scroll as f64 / track as f64)) as i32;
                                let new_offset = (start_offset as i32 + delta_offset)
                                    .clamp(0, max_scroll as i32)
                                    as u16;
                                app.session_mgr.current_mut().ui.scroll_offset = new_offset;
                                app.session_mgr.current_mut().ui.scroll_follow = false;
                            }
                        }
                    }
                }
                // Panel scrollbar drag: update panel scroll offset from mouse Y
                {
                    let session = &mut app.session_mgr.current_mut();
                    if session.ui.panel_scrollbar_dragging {
                        if let Some(ref metrics) = session.ui.panel_scrollbar_metrics {
                            let bar_inner_height = metrics.bar_area.height.saturating_sub(2);
                            if bar_inner_height > 0 {
                                let rel_y = (mouse.row.saturating_sub(metrics.bar_area.y + 1))
                                    .min(bar_inner_height);
                                let new_offset = ((rel_y as f64 / bar_inner_height as f64)
                                    * metrics.max_offset as f64)
                                    as u16;
                                let new_offset = new_offset.min(metrics.max_offset);
                                session
                                    .session_panels
                                    .dispatch_set_scroll_offset(new_offset);
                                session.ui.panel_scroll_offset = new_offset;
                            }
                        }
                        return Ok(Some(Action::Redraw));
                    }
                }
                // Panel selection drag
                if app.session_mgr.current_mut().ui.panel_selection.dragging {
                    if let Some(area) = app.session_mgr.current_mut().ui.panel_area {
                        let content_row = mouse
                            .row
                            .saturating_sub(area.y)
                            .saturating_add(app.session_mgr.current_mut().ui.panel_scroll_offset);
                        let col = mouse.column.saturating_sub(area.x);
                        app.session_mgr
                            .current_mut()
                            .ui
                            .panel_selection
                            .update_drag(content_row, col);
                    }
                }
                if app.session_mgr.current_mut().ui.text_selection.dragging {
                    if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                        let visual_row = mouse
                            .row
                            .saturating_sub(area.y)
                            .saturating_add(app.session_mgr.current_mut().ui.scroll_offset);
                        let visual_col = mouse.column.saturating_sub(area.x);
                        app.session_mgr
                            .current_mut()
                            .ui
                            .text_selection
                            .update_drag(visual_row, visual_col);
                    }
                }
                // Textarea area: extend textarea selection
                if app.session_mgr.current_mut().ui.textarea.is_selecting() {
                    if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                        if mouse.row >= area.y && mouse.row < area.y + area.height {
                            let session = &app.session_mgr.current_mut();
                            let (row, col) =
                                mouse::textarea_mouse_to_cursor(&session.ui.textarea, area, &mouse);
                            app.session_mgr.current_mut().ui.textarea.move_cursor(
                                tui_textarea::CursorMove::Jump(row as u16, col as u16),
                            );
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // End scrollbar drag
                app.session_mgr.current_mut().ui.scrollbar_dragging = false;
                // End panel scrollbar drag
                app.session_mgr.current_mut().ui.panel_scrollbar_dragging = false;
                // Panel selection released
                if app.session_mgr.current_mut().ui.panel_selection.dragging {
                    app.session_mgr.current_mut().ui.panel_selection.end_drag();
                    let sel = &app.session_mgr.current_mut().ui.panel_selection;
                    if let (Some(start), Some(end)) = (sel.start, sel.end) {
                        let text = crate::app::text_selection::extract_panel_text(
                            start,
                            end,
                            &app.session_mgr.current_mut().ui.panel_plain_lines,
                        );
                        app.session_mgr
                            .current_mut()
                            .ui
                            .panel_selection
                            .set_selected_text(text);
                    }
                    mouse::copy_panel_selection_to_clipboard(app);
                }
                if app.session_mgr.current_mut().ui.text_selection.dragging {
                    app.session_mgr.current_mut().ui.text_selection.end_drag();
                    let ts = &app.session_mgr.current_mut().ui.text_selection;
                    if let (Some(start), Some(end)) = (ts.start, ts.end) {
                        let usable_width = app
                            .session_mgr
                            .current_mut()
                            .ui
                            .messages_area
                            .map(|a| a.width.saturating_sub(1))
                            .unwrap_or(0);
                        let cache = app.session_mgr.current_mut().messages.render_cache.read();
                        let text = crate::app::text_selection::extract_selected_text(
                            start,
                            end,
                            &cache.wrap_map,
                            usable_width,
                        );
                        drop(cache);
                        app.session_mgr
                            .current_mut()
                            .ui
                            .text_selection
                            .set_selected_text(text);
                    }
                    mouse::copy_selection_to_clipboard(app);
                }
                // textarea selection on mouse up: no extra handling; tui_textarea maintains
                // its own selection state
            }
            _ => {}
        },
    }

    Ok(Some(Action::Redraw))
}

// ── OAuth prompt ────────────────────────────────────────────────────────────

fn handle_oauth_prompt(app: &mut App, input: Input) {
    let prompt = match app.global_ui.oauth_prompt.as_mut() {
        Some(p) => p,
        None => return,
    };
    match input {
        Input {
            key: Key::Enter, ..
        } => {
            if prompt.submit() {
                app.global_ui.oauth_prompt = None;
            }
        }
        Input {
            key: Key::Char('o'),
            ctrl: true,
            ..
        } => {
            let url = prompt.authorization_url.clone();
            #[cfg(unix)]
            let _ = std::process::Command::new("open").arg(&url).spawn();
            #[cfg(windows)]
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", &url])
                .spawn();
        }
        Input { key: Key::Esc, .. } => {
            app.global_ui.oauth_prompt = None;
        }
        Input {
            key: Key::Char('c'),
            ctrl: true,
            ..
        } => {
            // Ctrl+C in OAuth popup: ignore (no quit)
        }
        _ => {
            prompt.error_message = None;
            prompt.field.input(input);
        }
    }
}
