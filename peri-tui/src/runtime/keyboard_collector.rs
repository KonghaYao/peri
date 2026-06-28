//! Keyboard collector background task.
//!
//! Spawns a dedicated tokio task that polls crossterm for terminal events,
//! converts them to [`TuiEvent`], and pushes them into the single event channel.
//! This task is the **only** place in the codebase that touches crossterm's
//! blocking `event::poll` / `event::read` API.
//!
//! Design: <https://github.com/user/perihelion/blob/main/docs/design/peri-tui-architecture.md#71-from-input-to-event>

use std::time::Duration;

use ratatui::crossterm::event::{self, Event as CrosstermEvent, KeyEventKind};
use tokio_util::sync::CancellationToken;

use super::event_channel::{EventTx, TuiEvent};

/// Poll timeout used for crossterm event polling.
const POLL_TIMEOUT_MS: u64 = 50;

/// Interval between periodic [`TuiEvent::Tick`] pushes.
const TICK_INTERVAL: Duration = Duration::from_millis(POLL_TIMEOUT_MS);

/// Spawn the keyboard collector background task.
///
/// The task runs an internal loop that:
///
/// 1. Polls crossterm with a 50 ms timeout.
/// 2. On poll success, reads the event and converts it to [`TuiEvent`].
/// 3. Maintains a 50 ms interval timer that pushes [`TuiEvent::Tick`].
/// 4. Exits cleanly when `shutdown` is cancelled.
///
/// The task performs **no state mutation** -- pure conversion + channel push.
pub fn spawn(tx: EventTx, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { run(tx, shutdown).await })
}

async fn run(tx: EventTx, shutdown: CancellationToken) {
    let mut tick_interval = tokio::time::interval(TICK_INTERVAL);

    loop {
        tokio::select! {
            // Shutdown signal takes priority.
            _ = shutdown.cancelled() => {
                break;
            }

            // Periodic tick.
            _ = tick_interval.tick() => {
                let _ = tx.send(TuiEvent::Tick);
            }

            // Crossterm poll (blocking, but short -- 50 ms).  Spawn on
            // `spawn_blocking` so we don't hold up the async runtime.
            result = tokio::task::spawn_blocking(move || {
                if event::poll(Duration::from_millis(POLL_TIMEOUT_MS))
                    .unwrap_or(false)
                {
                    event::read().ok()
                } else {
                    None
                }
            }) => {
                match result {
                    Ok(Some(crossterm_ev)) => {
                        if let Some(tui_ev) = convert_crossterm_event(crossterm_ev) {
                            let _ = tx.send(tui_ev);
                        }
                    }
                    Ok(None) => {
                        // Poll timed out, no event available.
                    }
                    Err(_) => {
                        // spawn_blocking task panicked or was cancelled.
                        // Treat as shutdown to avoid spinning.
                        break;
                    }
                }
            }
        }
    }
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
