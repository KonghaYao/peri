use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;

use super::*;
use crate::api::{AgentMetrics, AgentMetricsSession, AgentMetricsTotals};
use crate::app::{ActionId, Effect, Page, Screen};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::metrics::{CoreMetricsSnapshot, TimeWindow};
use crate::ui::hit::HitMap;
use crate::ui::text;
use crate::worker::{ObservabilityOutcome, WorkerEvent};

const APP_KEY: &str = "ui-synthetic-key-never-real";
const OBS_PASSWORD: &str = "ui-observability-password-never-real";

fn app_with_observability() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let observability =
        ObservabilityConfig::new("http://127.0.0.1:5080", "default", "user", OBS_PASSWORD)
            .expect("observability");
    let config = Config::new(config::DEFAULT_BASE_URL, APP_KEY)
        .expect("config")
        .with_observability(observability);
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
}

fn app_without_observability() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, APP_KEY).expect("config");
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
}

fn core_snapshot() -> CoreMetricsSnapshot {
    CoreMetricsSnapshot {
        requests_per_minute: Some(12.5),
        error_rate: Some(0.02),
        p95_seconds: Some(0.42),
        total_requests: Some(500),
        window_start_us: 1_700_000_000_000_000,
        window_end_us: 1_700_000_900_000_000,
        covered_start_us: Some(1_700_000_100_000_000),
        covered_end_us: Some(1_700_000_800_000_000),
    }
}

fn empty_core_snapshot() -> CoreMetricsSnapshot {
    CoreMetricsSnapshot {
        window_start_us: 1,
        window_end_us: 2,
        ..CoreMetricsSnapshot::default()
    }
}

fn agent_metrics(count: usize, truncated: bool) -> AgentMetrics {
    AgentMetrics {
        start_time: 0,
        end_time: 0,
        sessions: (0..count)
            .map(|index| AgentMetricsSession {
                session_id: format!("sess_{index:03}"),
                agent_id: "root".to_owned(),
                model: Some("model-a".to_owned()),
                input_tokens: Some(10),
                output_tokens: None,
                cache_read_tokens: Some(2),
                cache_creation_tokens: Some(3),
                total_tokens: Some(13),
                turns: 1,
                calls: 1,
                last_active_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .collect(),
        totals: AgentMetricsTotals {
            input_tokens: Some(10),
            output_tokens: None,
            cache_read_tokens: Some(2),
            cache_creation_tokens: Some(3),
            total_tokens: Some(13),
            turns: 1,
            calls: 1,
            model: Some("model-a".to_owned()),
        },
        truncated,
    }
}

fn draw(app: &mut App, width: u16, height: u16) -> (Terminal<TestBackend>, HitMap) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let mut hits = HitMap::default();
    terminal
        .draw(|frame| hits = crate::ui::render(frame, app))
        .expect("draw");
    (terminal, hits)
}

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn load_core(app: &mut App, snapshot: CoreMetricsSnapshot) {
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    let (generation, window) = match app.metrics_due(std::time::Instant::now()).as_slice() {
        [Effect::LoadCoreMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected core request"),
    };
    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: ObservabilityOutcome::Ready(snapshot),
    });
}

fn load_agent(app: &mut App, metrics: AgentMetrics) {
    app.apply(Intent::SelectPage(Page::AgentMetrics));
    let (generation, window) = match app.metrics_due(std::time::Instant::now()).as_slice() {
        [Effect::LoadAgentMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected agent request"),
    };
    app.handle_worker(WorkerEvent::AgentMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: Ok(metrics),
    });
}

fn load_request_log(app: &mut App, rows: Vec<crate::metrics::RequestLogRow>, has_next: bool) {
    app.apply(Intent::SelectPage(Page::RequestLog));
    let (generation, window, page) = match app.metrics_due(std::time::Instant::now()).as_slice() {
        [Effect::LoadRequestLog {
            generation,
            window,
            page,
            ..
        }] => (*generation, *window, *page),
        _ => panic!("expected request-log request"),
    };
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page,
        result: ObservabilityOutcome::Ready(crate::metrics::RequestLogPage {
            rows,
            page,
            has_next,
            window_start_us: 0,
            window_end_us: 1,
        }),
    });
}

#[test]
fn core_metrics_renders_values_and_window_chips() {
    let (mut app, _dir) = app_with_observability();
    load_core(&mut app, core_snapshot());
    let (terminal, hits) = draw(&mut app, 110, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::CORE_METRICS_TITLE));
    assert!(text.contains(text::LABEL_REQUESTS_PER_MINUTE));
    assert!(text.contains("12.5"));
    assert!(text.contains("2.0%"));
    assert!(text.contains("420 ms"));
    assert!(text.contains("500"));
    assert!(!text.contains(APP_KEY));
    assert!(!text.contains(OBS_PASSWORD));

    let intent = Intent::SetMetricsWindow(TimeWindow::OneHour);
    let rect = hits.region_rect(intent.clone()).expect("window chip");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(intent.clone()));
    app.apply(intent);
    assert_eq!(app.core_metrics().window(), TimeWindow::OneHour);

    let refresh = Intent::Activate(ActionId::RefreshCoreMetrics);
    assert!(hits.region_rect(refresh).is_some());
}

#[test]
fn core_metrics_renders_empty_and_error_states() {
    let (mut app, _dir) = app_with_observability();
    load_core(&mut app, empty_core_snapshot());
    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::EMPTY_CORE_METRICS));

    let (mut app, _dir) = app_with_observability();
    load_core(&mut app, core_snapshot());
    let (generation, window) = match app
        .metrics_due(std::time::Instant::now() + std::time::Duration::from_secs(15))
        .as_slice()
    {
        [Effect::LoadCoreMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected refresh"),
    };
    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: ObservabilityOutcome::Failed(crate::observability::ObservabilityError::Unreachable),
    });
    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains("Could not load core metrics"));
    assert!(text.contains(text::STALE_DATA));
    assert!(text.contains("12.5"), "last good data stays visible");
}

#[test]
fn core_metrics_renders_the_not_configured_state_with_a_settings_button() {
    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    assert!(app.metrics_due(std::time::Instant::now()).is_empty());
    let (_terminal, hits) = draw(&mut app, 100, 30);
    assert!(hits
        .region_rect(Intent::Activate(ActionId::OpenSettings))
        .is_some());
    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::OBSERVABILITY_NOT_CONFIGURED_MESSAGE));
    assert!(text.contains(text::action_label(ActionId::OpenSettings)));
}

#[test]
fn agent_metrics_renders_totals_table_and_truncation() {
    let (mut app, _dir) = app_without_observability();
    load_agent(&mut app, agent_metrics(25, true));
    let (terminal, hits) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::AGENT_METRICS_TITLE));
    assert!(text.contains(text::AGENT_COL_SESSION));
    assert!(text.contains(text::AGENT_COL_MODEL));
    assert!(text.contains(text::AGENT_COL_INPUT));
    assert!(text.contains(text::AGENT_COL_OUTPUT));
    assert!(text.contains(text::AGENT_COL_CACHE));
    assert!(text.contains(text::AGENT_COL_TOTAL));
    assert!(text.contains(text::AGENT_COL_TURNS));
    assert!(text.contains(text::AGENT_COL_LAST_ACTIVE));
    assert!(text.contains("sess_000"));
    assert!(text.contains(text::AGENT_METRICS_TRUNCATED));
    assert!(text.contains("Page 1 / 3"));
    assert!(!text.contains(APP_KEY));
    assert!(hits
        .region_rect(Intent::Activate(ActionId::RefreshAgentMetrics))
        .is_some());

    let next = Intent::MetricsPageNext;
    assert!(hits.region_rect(next.clone()).is_some());
    app.apply(next);
    let (terminal, _) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("sess_010"));
    assert!(!text.contains("sess_000"));
}

#[test]
fn agent_metrics_renders_empty_and_error_states() {
    let (mut app, _dir) = app_without_observability();
    load_agent(&mut app, agent_metrics(0, false));
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains(text::EMPTY_AGENT_METRICS));

    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::AgentMetrics));
    let (generation, window) = match app.metrics_due(std::time::Instant::now()).as_slice() {
        [Effect::LoadAgentMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected agent request"),
    };
    app.handle_worker(WorkerEvent::AgentMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: Err(crate::api::ApiError::local("gateway down")),
    });
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains("Could not load agent metrics"));
}

#[test]
fn request_log_renders_all_columns_and_missing_values() {
    let (mut app, _dir) = app_with_observability();
    let rows = vec![
        crate::metrics::RequestLogRow {
            timestamp_us: Some(1_700_000_000_000_000),
            method: Some("GET".to_owned()),
            route: Some("/v1/agents/sessions".to_owned()),
            status: Some(200),
            duration_ms: Some(12),
            session_id: Some("sess_a".to_owned()),
        },
        crate::metrics::RequestLogRow {
            timestamp_us: None,
            method: None,
            route: Some("/v1/agents/metrics".to_owned()),
            status: Some(500),
            duration_ms: None,
            session_id: None,
        },
    ];
    load_request_log(&mut app, rows, true);
    let (terminal, hits) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::REQUEST_LOG_TITLE));
    assert!(text.contains(text::REQUEST_COL_TIME));
    assert!(text.contains(text::REQUEST_COL_METHOD));
    assert!(text.contains(text::REQUEST_COL_ROUTE));
    assert!(text.contains(text::REQUEST_COL_STATUS));
    assert!(text.contains(text::REQUEST_COL_DURATION));
    assert!(text.contains(text::REQUEST_COL_SESSION));
    assert!(text.contains("/v1/agents/sessions"));
    assert!(text.contains("200"));
    assert!(text.contains("12 ms"));
    assert!(text.contains("sess_a"));
    assert!(text.contains(text::DASH));
    assert!(!text.contains(APP_KEY));
    assert!(!text.contains(OBS_PASSWORD));

    assert!(hits.region_rect(Intent::MetricsPageNext).is_some());
    assert!(hits.region_rect(Intent::MetricsPagePrev).is_none());
    assert!(hits
        .region_rect(Intent::Activate(ActionId::RefreshRequestLog))
        .is_some());
}

#[test]
fn request_log_renders_empty_error_and_not_configured_states() {
    let (mut app, _dir) = app_with_observability();
    load_request_log(&mut app, Vec::new(), false);
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains(text::EMPTY_REQUEST_LOG));

    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::RequestLog));
    let (generation, window, page) = match app.metrics_due(std::time::Instant::now()).as_slice() {
        [Effect::LoadRequestLog {
            generation,
            window,
            page,
            ..
        }] => (*generation, *window, *page),
        _ => panic!("expected request-log request"),
    };
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page,
        result: ObservabilityOutcome::Failed(crate::observability::ObservabilityError::Timeout),
    });
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains("Could not load requests"));

    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::RequestLog));
    assert!(app.metrics_due(std::time::Instant::now()).is_empty());
    let (_terminal, hits) = draw(&mut app, 100, 30);
    assert!(hits
        .region_rect(Intent::Activate(ActionId::OpenSettings))
        .is_some());
}

#[test]
fn metrics_pages_render_without_panicking_at_sixty_columns() {
    let (mut app, _dir) = app_with_observability();
    load_core(&mut app, core_snapshot());
    draw(&mut app, 60, 24);

    let (mut app, _dir) = app_without_observability();
    load_agent(&mut app, agent_metrics(25, true));
    draw(&mut app, 60, 24);

    let (mut app, _dir) = app_with_observability();
    load_request_log(
        &mut app,
        vec![crate::metrics::RequestLogRow {
            timestamp_us: Some(1_700_000_000_000_000),
            method: Some("GET".to_owned()),
            route: Some("/v1/agents/sessions".to_owned()),
            status: Some(200),
            duration_ms: Some(12),
            session_id: Some("sess_a".to_owned()),
        }],
        true,
    );
    draw(&mut app, 60, 24);
    assert_eq!(app.screen(), Screen::Main);
}

#[test]
fn sandbox_metrics_keeps_the_honest_empty_page() {
    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::SandboxMetrics));
    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::empty_page(Page::SandboxMetrics).reason));
    assert!(text.contains(text::EMPTY_NOTICE));
    assert!(!Page::SandboxMetrics.is_implemented());
}
