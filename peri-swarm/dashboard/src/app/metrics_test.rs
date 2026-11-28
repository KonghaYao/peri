use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::api::{AgentMetrics, AgentMetricsSession, AgentMetricsTotals};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::metrics::{CoreMetricsSnapshot, RequestLogPage, RequestLogRow, AGENT_METRICS_PAGE_SIZE};
use crate::test_support::connected_app;
use crate::worker::{ObservabilityOutcome, WorkerEvent};

fn app_with_observability() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let observability =
        ObservabilityConfig::new("http://127.0.0.1:5080", "default", "user", "pass")
            .expect("observability");
    let config = Config::new(config::DEFAULT_BASE_URL, "synthetic-key")
        .expect("config")
        .with_observability(observability);
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
}

fn core_snapshot() -> CoreMetricsSnapshot {
    CoreMetricsSnapshot {
        requests_per_minute: Some(12.5),
        error_rate: Some(0.02),
        p95_seconds: Some(0.42),
        total_requests: Some(500),
        window_start_us: 1_000_000,
        window_end_us: 2_000_000,
        covered_start_us: Some(1_100_000),
        covered_end_us: Some(2_000_000),
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
                input_tokens: Some(10 + index as u64),
                output_tokens: None,
                cache_read_tokens: Some(index as u64),
                cache_creation_tokens: Some(1),
                total_tokens: Some(11 + index as u64),
                turns: 1,
                calls: 1,
                last_active_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .collect(),
        totals: AgentMetricsTotals {
            input_tokens: Some(100),
            output_tokens: None,
            cache_read_tokens: Some(5),
            cache_creation_tokens: Some(6),
            total_tokens: Some(111),
            turns: 3,
            calls: 4,
            model: Some("model-a".to_owned()),
        },
        truncated,
    }
}

fn request_page(page: usize, count: usize, has_next: bool) -> RequestLogPage {
    RequestLogPage {
        rows: (0..count)
            .map(|index| RequestLogRow {
                timestamp_us: Some(1_700_000_000_000_000 + index as u64),
                method: Some("GET".to_owned()),
                route: Some("/v1/agents/sessions".to_owned()),
                status: Some(200),
                duration_ms: Some(5),
                session_id: Some(format!("sess_{index}")),
            })
            .collect(),
        page,
        has_next,
        window_start_us: 0,
        window_end_us: 1,
    }
}

fn core_request(effects: &[Effect]) -> (u64, TimeWindow) {
    match effects {
        [Effect::LoadCoreMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected one LoadCoreMetrics effect"),
    }
}

fn agent_request(effects: &[Effect]) -> (u64, TimeWindow) {
    match effects {
        [Effect::LoadAgentMetrics {
            generation, window, ..
        }] => (*generation, *window),
        _ => panic!("expected one LoadAgentMetrics effect"),
    }
}

fn request_log_request(effects: &[Effect]) -> (u64, TimeWindow, usize) {
    match effects {
        [Effect::LoadRequestLog {
            generation,
            window,
            page,
            ..
        }] => (*generation, *window, *page),
        _ => panic!("expected one LoadRequestLog effect"),
    }
}

#[test]
fn entering_core_metrics_loads_then_polls_without_overlap() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    let now = Instant::now();
    let (generation, window) = core_request(&app.metrics_due(now));
    assert_eq!(window, TimeWindow::FifteenMinutes);
    assert!(app.metrics_due(now).is_empty(), "no overlapping request");

    let effects = app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: ObservabilityOutcome::Ready(core_snapshot()),
    });
    assert!(effects.is_empty());
    assert!(app.core_metrics().snapshot().is_some());
    assert!(!app.core_metrics().loading());

    assert!(app.metrics_due(now + Duration::from_secs(14)).is_empty());
    let (next_generation, _) = core_request(&app.metrics_due(now + Duration::from_secs(15)));
    assert_eq!(next_generation, generation);
}

#[test]
fn changing_the_core_window_discards_the_old_generation() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    let (generation, window) = core_request(&app.metrics_due(Instant::now()));

    assert!(app
        .apply(Intent::SetMetricsWindow(TimeWindow::OneHour))
        .is_empty());
    assert_eq!(app.core_metrics().window(), TimeWindow::OneHour);
    assert!(app.core_metrics().snapshot().is_none());

    let effects = app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: ObservabilityOutcome::Ready(core_snapshot()),
    });
    let (next_generation, next_window) = core_request(&effects);
    assert_eq!(next_window, TimeWindow::OneHour);
    assert!(next_generation > generation);
    assert!(
        app.core_metrics().snapshot().is_none(),
        "stale window data must not be applied"
    );

    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation: next_generation,
        window: next_window,
        result: ObservabilityOutcome::Ready(core_snapshot()),
    });
    assert!(app.core_metrics().snapshot().is_some());
}

#[test]
fn leaving_metrics_pages_stops_scheduling() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    core_request(&app.metrics_due(Instant::now()));
    app.apply(Intent::SelectPage(Page::SessionLog));
    assert!(app.metrics_due(Instant::now()).is_empty());

    app.apply(Intent::SelectPage(Page::AgentMetrics));
    agent_request(&app.metrics_due(Instant::now()));
    app.apply(Intent::SelectPage(Page::SessionLog));
    assert!(app.metrics_due(Instant::now()).is_empty());

    app.apply(Intent::SelectPage(Page::RequestLog));
    request_log_request(&app.metrics_due(Instant::now()));
    app.apply(Intent::SelectPage(Page::SessionLog));
    assert!(app.metrics_due(Instant::now()).is_empty());
}

#[test]
fn agent_metrics_loads_without_openobserve_and_pages_client_side() {
    let (mut app, _dir) = connected_app();
    assert!(!app.observability_configured());
    app.apply(Intent::SelectPage(Page::AgentMetrics));
    let (generation, window) = agent_request(&app.metrics_due(Instant::now()));
    assert_eq!(window, TimeWindow::FifteenMinutes);

    app.handle_worker(WorkerEvent::AgentMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: Ok(agent_metrics(25, true)),
    });
    assert_eq!(app.agent_metrics().page_count(), 3);
    assert_eq!(
        app.agent_metrics().page_rows().len(),
        AGENT_METRICS_PAGE_SIZE
    );
    assert_eq!(app.agent_metrics().page(), 0);
    assert!(app.agent_metrics().truncated());

    assert!(app.apply(Intent::MetricsPageNext).is_empty());
    assert_eq!(app.agent_metrics().page(), 1);
    app.apply(Intent::MetricsPageNext);
    assert_eq!(app.agent_metrics().page(), 2);
    app.apply(Intent::MetricsPageNext);
    assert_eq!(app.agent_metrics().page(), 2, "last page is a boundary");
    app.apply(Intent::MetricsPagePrev);
    assert_eq!(app.agent_metrics().page(), 1);
}

#[test]
fn changing_the_agent_window_resets_the_page_and_reloads() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::SelectPage(Page::AgentMetrics));
    let (generation, window) = agent_request(&app.metrics_due(Instant::now()));
    app.handle_worker(WorkerEvent::AgentMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: Ok(agent_metrics(25, false)),
    });
    app.apply(Intent::MetricsPageNext);
    assert_eq!(app.agent_metrics().page(), 1);

    let (generation, window) =
        agent_request(&app.apply(Intent::SetMetricsWindow(TimeWindow::OneHour)));
    assert_eq!(window, TimeWindow::OneHour);
    assert_eq!(app.agent_metrics().page(), 0);
    assert!(app.agent_metrics().metrics().is_none());
    assert!(generation > 0);
}

#[test]
fn request_log_and_core_show_not_configured_without_a_request() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    assert!(app.metrics_due(Instant::now()).is_empty());
    assert!(app.core_metrics().not_configured());
    assert!(!app.core_metrics().loading());

    app.apply(Intent::SelectPage(Page::RequestLog));
    assert!(app.metrics_due(Instant::now()).is_empty());
    assert!(app.request_log().not_configured());
    assert!(!app.request_log().loading());
}

#[test]
fn request_log_paging_requests_and_discards_stale_pages() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::RequestLog));
    let (generation, window, page) = request_log_request(&app.metrics_due(Instant::now()));
    assert_eq!((window, page), (TimeWindow::FifteenMinutes, 0));
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page,
        result: ObservabilityOutcome::Ready(request_page(0, 10, true)),
    });
    assert_eq!(app.request_log().rows().len(), 10);
    assert!(app.request_log().can_next());

    let (next_generation, _, next_page) = request_log_request(&app.apply(Intent::MetricsPageNext));
    assert_eq!(next_page, 1);
    assert!(next_generation > generation);

    let effects = app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page: 0,
        result: ObservabilityOutcome::Ready(request_page(0, 10, true)),
    });
    let (reload_generation, _, reload_page) = request_log_request(&effects);
    assert_eq!(reload_page, 1);
    assert_eq!(reload_generation, next_generation);
    assert_eq!(
        app.request_log().applied_page(),
        0,
        "stale page must not be applied"
    );

    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation: next_generation,
        window,
        page: 1,
        result: ObservabilityOutcome::Ready(request_page(1, 2, false)),
    });
    assert_eq!(app.request_log().applied_page(), 1);
    assert!(!app.request_log().can_next());
    assert!(app.apply(Intent::MetricsPageNext).is_empty());

    let (previous_generation, _, previous_page) =
        request_log_request(&app.apply(Intent::MetricsPagePrev));
    assert_eq!(previous_page, 0);
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation: previous_generation,
        window,
        page: 0,
        result: ObservabilityOutcome::Ready(request_page(0, 2, true)),
    });
    assert_eq!(app.request_log().applied_page(), 0);
}

#[test]
fn request_log_failure_keeps_the_applied_page() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::RequestLog));
    let (generation, window, page) = request_log_request(&app.metrics_due(Instant::now()));
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page,
        result: ObservabilityOutcome::Ready(request_page(0, 10, true)),
    });
    let (next_generation, _, next_page) = request_log_request(&app.apply(Intent::MetricsPageNext));
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation: next_generation,
        window,
        page: next_page,
        result: ObservabilityOutcome::Failed(crate::observability::ObservabilityError::Unreachable),
    });
    assert_eq!(app.request_log().page(), 0);
    assert_eq!(app.request_log().applied_page(), 0);
    assert_eq!(app.request_log().rows().len(), 10);
    assert!(app.request_log().stale());
    assert!(app.request_log().error().is_some());
}

#[test]
fn core_failure_keeps_the_last_snapshot_and_marks_it_stale() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    let now = Instant::now();
    let (generation, window) = core_request(&app.metrics_due(now));
    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: ObservabilityOutcome::Ready(core_snapshot()),
    });
    let (next_generation, _) = core_request(&app.metrics_due(now + Duration::from_secs(15)));
    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation: next_generation,
        window,
        result: ObservabilityOutcome::Failed(crate::observability::ObservabilityError::Unreachable),
    });
    assert!(app.core_metrics().snapshot().is_some());
    assert!(app.core_metrics().stale());
    assert!(app
        .core_metrics()
        .error()
        .is_some_and(|error| error.contains("Could not reach OpenObserve")));
}

#[test]
fn not_configured_results_from_the_worker_mark_both_pages() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    app.handle_worker(WorkerEvent::CoreMetricsLoaded {
        epoch: app.epoch(),
        generation: 0,
        window: TimeWindow::FifteenMinutes,
        result: ObservabilityOutcome::NotConfigured,
    });
    assert!(app.core_metrics().not_configured());

    app.apply(Intent::SelectPage(Page::RequestLog));
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation: 0,
        window: TimeWindow::FifteenMinutes,
        page: 0,
        result: ObservabilityOutcome::NotConfigured,
    });
    assert!(app.request_log().not_configured());
}

#[test]
fn agent_metrics_401_pauses_polling_and_shows_the_gateway_banner() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::SelectPage(Page::AgentMetrics));
    let (generation, window) = agent_request(&app.metrics_due(Instant::now()));
    let error = crate::api::ApiError::from_status(
        401,
        br#"{"error":{"code":"unauthorized","message":"bad key","details":null}}"#,
    );
    app.handle_worker(WorkerEvent::AgentMetricsLoaded {
        epoch: app.epoch(),
        generation,
        window,
        result: Err(error),
    });
    assert!(app.session_log().poll.is_paused());
    assert!(app.agent_metrics().poll.is_paused());
    assert!(app
        .agent_metrics()
        .error()
        .is_some_and(|error| error.contains("Could not load agent metrics")));
    assert!(app
        .active_banner()
        .is_some_and(|banner| banner.unauthorized));
    assert!(!app
        .agent_metrics()
        .error()
        .unwrap_or_default()
        .contains("synthetic-key"));
}

#[test]
fn windows_are_selectable_from_the_keyboard() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::CoreMetrics));
    let _ = app.metrics_due(Instant::now());
    app.handle_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('2'),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.core_metrics().window(), TimeWindow::OneHour);
}
