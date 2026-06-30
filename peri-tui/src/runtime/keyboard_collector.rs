//! Keyboard collector background task.
//!
//! Spawns a dedicated tokio task that polls crossterm for terminal events,
//! converts them to [`TuiEvent`], and pushes them into the single event channel.
//! This task is the **only** place in the codebase that touches crossterm's
//! blocking `event::poll` / `event::read` API.
//!
//! ## Event-loss fix (2026-06-30)
//!
//! The original implementation used `tokio::select!` with a `spawn_blocking`
//! future racing against a tick interval. Both had 50 ms timeouts, so they
//! resolved simultaneously. `select!` (non-biased) picks one at random --
//! when tick won, the `JoinHandle` was dropped. Dropping a `JoinHandle`
//! **detaches** the task (it keeps running), the detached task called
//! `event::read()` and consumed the crossterm event, but the result was lost.
//!
//! The fix: a persistent `spawn_blocking` task that continuously polls
//! crossterm and pushes events through an mpsc channel. The async loop
//! reads from this channel with `tokio::select!` -- the channel's `recv()`
//! future is poll-based (no detached task), so dropping it is safe.
//!
//! Design: `docs/design/peri-tui-architecture.md` §7.1

use std::time::Duration;

use ratatui::crossterm::event::{self, Event as CrosstermEvent, KeyEventKind};
use tokio_util::sync::CancellationToken;

use super::event_channel::{EventTx, TuiEvent};

/// Poll timeout used for crossterm event polling in the background task.
const POLL_TIMEOUT_MS: u64 = 50;

/// Interval between periodic [`TuiEvent::Tick`] pushes.
const TICK_INTERVAL: Duration = Duration::from_millis(POLL_TIMEOUT_MS);

/// Spawn the keyboard collector background task.
///
/// Architecture:
///
/// 1. A **persistent** `spawn_blocking` task continuously polls crossterm
///    and pushes raw `CrosstermEvent`s into an unbounded mpsc channel.
///    This task is never cancelled mid-poll, so events cannot be lost.
/// 2. An async task reads from the mpsc channel and the tick interval
///    via `tokio::select!`. The channel `recv()` is poll-based -- no
///    detached task means no event loss when another branch wins.
/// 3. Both tasks exit when `shutdown` is cancelled.
pub fn spawn(tx: EventTx, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
    let (ct_tx, mut ct_rx) = tokio::sync::mpsc::unbounded_channel::<CrosstermEvent>();

    // Persistent crossterm poller. Runs on the blocking thread pool and
    // is NEVER cancelled mid-read -- the while-loop only exits when the
    // shutdown token is cancelled.
    let ct_shutdown = shutdown.clone();
    tokio::task::spawn_blocking(move || {
        while !ct_shutdown.is_cancelled() {
            if event::poll(Duration::from_millis(POLL_TIMEOUT_MS)).unwrap_or(false) {
                if let Ok(ev) = event::read() {
                    // Send is best-effort; if the channel is closed the
                    // async task has already dropped its receiver.
                    let _ = ct_tx.send(ev);
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut tick_interval = tokio::time::interval(TICK_INTERVAL);
        // Suppress the initial immediate tick burst.
        tick_interval.reset_after(TICK_INTERVAL);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    break;
                }

                _ = tick_interval.tick() => {
                    let _ = tx.send(TuiEvent::Tick);
                }

                Some(crossterm_ev) = ct_rx.recv() => {
                    if let Some(tui_ev) = convert_crossterm_event(crossterm_ev) {
                        let _ = tx.send(tui_ev);
                    }
                }
            }
        }
    })
}

/// Convert a crossterm [`CrosstermEvent`] into a [`TuiEvent`], returning `None`
/// for events that should be silently dropped.
fn convert_crossterm_event(ev: CrosstermEvent) -> Option<TuiEvent> {
    match ev {
        CrosstermEvent::Key(key_event) => {
            // Drop key release events -- they generate noise (especially on
            // terminals that send both press and release for every keystroke).
            if key_event.kind == KeyEventKind::Release {
                return None;
            }
            Some(TuiEvent::Key(key_event))
        }

        CrosstermEvent::Mouse(mouse_event) => Some(TuiEvent::Mouse(mouse_event)),

        CrosstermEvent::Paste(text) => {
            // Normalise line separators: some terminals (e.g. VSCode) use \r.
            let text = text.replace('\r', "\n");
            Some(TuiEvent::Paste(text))
        }

        CrosstermEvent::Resize(columns, rows) => Some(TuiEvent::Resize(columns, rows)),

        CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => {
            // Focus events are not needed in the v2 architecture.  The main
            // loop does not maintain a `focused` flag -- rendering is driven
            // solely by TuiEvent arrivals and tick-based throttle checks.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_key_press() {
        let key = event::KeyEvent::new(event::KeyCode::Char('a'), event::KeyModifiers::NONE);
        let crossterm_ev = CrosstermEvent::Key(key);
        let result = convert_crossterm_event(crossterm_ev);
        assert!(result.is_some());
        match result.unwrap() {
            TuiEvent::Key(k) => {
                assert_eq!(k.code, event::KeyCode::Char('a'));
                assert_eq!(k.modifiers, event::KeyModifiers::NONE);
            }
            other => panic!("期望 TuiEvent::Key，实际 {other:?}"),
        }
    }

    #[test]
    fn test_convert_key_release_dropped() {
        let key = event::KeyEvent {
            code: event::KeyCode::Char('a'),
            modifiers: event::KeyModifiers::NONE,
            kind: event::KeyEventKind::Release,
            state: event::KeyEventState::NONE,
        };
        let crossterm_ev = CrosstermEvent::Key(key);
        assert!(convert_crossterm_event(crossterm_ev).is_none());
    }

    #[test]
    fn test_convert_mouse() {
        let mouse = event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: event::KeyModifiers::NONE,
        };
        let crossterm_ev = CrosstermEvent::Mouse(mouse);
        let result = convert_crossterm_event(crossterm_ev);
        assert!(result.is_some());
        match result.unwrap() {
            TuiEvent::Mouse(m) => {
                assert_eq!(m.column, 10);
                assert_eq!(m.row, 5);
            }
            other => panic!("期望 TuiEvent::Mouse，实际 {other:?}"),
        }
    }

    #[test]
    fn test_convert_paste_normalises_cr() {
        let crossterm_ev = CrosstermEvent::Paste("hello\rworld".into());
        let result = convert_crossterm_event(crossterm_ev);
        assert!(result.is_some());
        match result.unwrap() {
            TuiEvent::Paste(text) => {
                assert_eq!(text, "hello\nworld");
            }
            other => panic!("期望 TuiEvent::Paste，实际 {other:?}"),
        }
    }

    #[test]
    fn test_convert_resize() {
        let crossterm_ev = CrosstermEvent::Resize(120, 40);
        let result = convert_crossterm_event(crossterm_ev);
        assert!(result.is_some());
        match result.unwrap() {
            TuiEvent::Resize(cols, rows) => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            other => panic!("期望 TuiEvent::Resize，实际 {other:?}"),
        }
    }

    #[test]
    fn test_convert_focus_gained_dropped() {
        let crossterm_ev = CrosstermEvent::FocusGained;
        assert!(convert_crossterm_event(crossterm_ev).is_none());
    }

    #[test]
    fn test_convert_focus_lost_dropped() {
        let crossterm_ev = CrosstermEvent::FocusLost;
        assert!(convert_crossterm_event(crossterm_ev).is_none());
    }
}
