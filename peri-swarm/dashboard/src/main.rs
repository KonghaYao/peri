//! Terminal lifecycle and the dashboard event loop.
//!
//! The UI thread owns the terminal and the [`App`]. A dedicated worker thread
//! owns the HTTP client; commands and events flow over channels so a slow
//! gateway can never block input, rendering, or terminal restoration.

use std::io::{self, Stdout};
use std::panic;
use std::time::{Duration, Instant};

use peri_swarm_dashboard::app::{App, Effect};
use peri_swarm_dashboard::config::ConfigStore;
use peri_swarm_dashboard::ui;
use peri_swarm_dashboard::ui::hit::HitMap;
use peri_swarm_dashboard::ui::mouse::{handle_mouse, ScrollDrag};
use peri_swarm_dashboard::ui::text as strings;
use peri_swarm_dashboard::worker::{Command, WorkerHandle};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute, terminal};
use ratatui::Terminal;

/// Input poll cadence.
const EVENT_POLL: Duration = Duration::from_millis(50);
/// Repaint cadence for clock and health driven views.
const TICK: Duration = Duration::from_millis(250);

fn main() -> std::process::ExitCode {
    install_panic_hook();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", strings::STARTUP_ERROR);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let store = ConfigStore::from_home().map_err(|error| {
        io::Error::other(strings::config_read_failed(&strings::config_error_detail(
            &error,
        )))
    })?;
    let (app, initial) = match store.load() {
        Ok(Some(loaded)) => {
            let (config, warnings) = loaded.into_parts();
            let initial = config.clone();
            (
                App::connected(config, warnings, store.clone()),
                Some(initial),
            )
        }
        Ok(None) => (App::onboarding(store.clone()), None),
        Err(error) => (
            App::onboarding_with_error(store.clone(), Some(strings::config_error_detail(&error))),
            None,
        ),
    };
    let mut worker = WorkerHandle::spawn(initial);

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    run_loop(&mut terminal, app, &mut worker)
}

/// Restore the terminal before the default panic output is printed.
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Alternate screen, raw mode, mouse capture and bracketed paste.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            cursor::Hide
        ) {
            // A partially applied sequence must not strand the terminal.
            restore_terminal();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
    worker: &mut WorkerHandle,
) -> io::Result<()> {
    let mut dirty = true;
    let mut last_tick = Instant::now();
    let mut hits = HitMap::default();
    let mut drag: Option<ScrollDrag> = None;

    loop {
        while let Some(worker_event) = worker.try_event() {
            let effects = app.handle_worker(worker_event);
            execute(effects, worker);
            dirty = true;
        }
        if let Some(effect) = app.health_poll_due(Instant::now()) {
            execute(vec![effect], worker);
            dirty = true;
        }
        if let Some(effect) = app.session_list_due(Instant::now()) {
            execute(vec![effect], worker);
            dirty = true;
        }
        let metrics_effects = app.metrics_due(Instant::now());
        if !metrics_effects.is_empty() {
            execute(metrics_effects, worker);
            dirty = true;
        }
        let detail_effects = app.detail_due(Instant::now());
        if !detail_effects.is_empty() {
            execute(detail_effects, worker);
            dirty = true;
        }
        if app.should_quit() {
            return Ok(());
        }

        if dirty {
            terminal.draw(|frame| {
                hits = ui::render(frame, &mut app);
            })?;
            dirty = false;
        }

        let until_tick = TICK.saturating_sub(last_tick.elapsed());
        let timeout = until_tick.min(EVENT_POLL);
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    let effects = app.handle_key(key);
                    execute(effects, worker);
                    dirty = true;
                }
                Event::Paste(text) => {
                    app.handle_paste(&text);
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    let effects = handle_mouse(&mut app, &hits, &mut drag, mouse);
                    execute(effects, worker);
                    dirty = true;
                }
                Event::Resize(_, _) => {
                    // A resize can reflow the frame the terminal already had,
                    // so the diff against the previous buffer is not reliable.
                    // Clearing forces a full repaint and prevents stale glyphs
                    // (for example a leftover header character).
                    terminal.clear()?;
                    dirty = true;
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.advance_spinner();
            dirty = true;
        }
    }
}

fn execute(effects: Vec<Effect>, worker: &WorkerHandle) {
    for effect in effects {
        match effect {
            Effect::TestObservability {
                epoch,
                base_url,
                org,
                user,
                password,
            } => worker.send(Command::TestObservability {
                epoch,
                base_url,
                org,
                user,
                password,
            }),
            Effect::Validate {
                epoch,
                purpose,
                base_url,
                api_key,
            } => worker.send(Command::Validate {
                epoch,
                purpose,
                base_url,
                api_key,
            }),
            Effect::Revalidate { epoch, purpose } => {
                worker.send(Command::Revalidate { epoch, purpose });
            }
            Effect::Adopt { base_url, api_key } => {
                worker.send(Command::Adopt { base_url, api_key });
            }
            Effect::AdoptObservability {
                base_url,
                org,
                user,
                password,
            } => worker.send(Command::AdoptObservability {
                base_url,
                org,
                user,
                password,
            }),
            Effect::Health { epoch } => worker.send(Command::Health { epoch }),
            Effect::ListSessions {
                epoch,
                generation,
                limit,
            } => worker.send(Command::ListSessions {
                epoch,
                generation,
                limit,
            }),
            Effect::LoadCoreMetrics {
                epoch,
                generation,
                window,
            } => worker.send(Command::LoadCoreMetrics {
                epoch,
                generation,
                window,
            }),
            Effect::LoadAgentMetrics {
                epoch,
                generation,
                window,
            } => worker.send(Command::LoadAgentMetrics {
                epoch,
                generation,
                window,
            }),
            Effect::LoadRequestLog {
                epoch,
                generation,
                window,
                page,
            } => worker.send(Command::LoadRequestLog {
                epoch,
                generation,
                window,
                page,
            }),
            Effect::RunQuery {
                epoch,
                generation,
                sql,
                start_us,
                end_us,
                from,
                size,
            } => worker.send(Command::RunQuery {
                epoch,
                generation,
                sql,
                start_us,
                end_us,
                from,
                size,
            }),
            Effect::CreateSession { epoch, spec, key } => {
                worker.send(Command::CreateSession { epoch, spec, key });
            }
            Effect::GetSession {
                epoch,
                generation,
                session_id,
            } => worker.send(Command::GetSession {
                epoch,
                generation,
                session_id,
            }),
            Effect::ListItems {
                epoch,
                generation,
                session_id,
                limit,
                after,
            } => worker.send(Command::ListItems {
                epoch,
                generation,
                session_id,
                limit,
                after,
            }),
            Effect::ListTurns {
                epoch,
                generation,
                session_id,
                limit,
                after,
            } => worker.send(Command::ListTurns {
                epoch,
                generation,
                session_id,
                limit,
                after,
            }),
            Effect::UpdateSession {
                epoch,
                session_id,
                spec,
                key,
            } => worker.send(Command::UpdateSession {
                epoch,
                session_id,
                spec,
                key,
            }),
            Effect::DeleteSession {
                epoch,
                session_id,
                key,
            } => worker.send(Command::DeleteSession {
                epoch,
                session_id,
                key,
            }),
            Effect::SendInput {
                epoch,
                session_id,
                text,
                key,
            } => worker.send(Command::SendInput {
                epoch,
                session_id,
                text,
                key,
            }),
            Effect::CancelTurn {
                epoch,
                session_id,
                key,
            } => worker.send(Command::CancelTurn {
                epoch,
                session_id,
                key,
            }),
            Effect::AnswerApproval {
                epoch,
                session_id,
                request_id,
                decision,
                key,
            } => worker.send(Command::AnswerApproval {
                epoch,
                session_id,
                request_id,
                decision,
                key,
            }),
            Effect::DropClient => worker.send(Command::SignOut),
            Effect::Quit => {}
        }
    }
}
