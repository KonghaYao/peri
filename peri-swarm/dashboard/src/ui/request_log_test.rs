use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;

use super::*;
use crate::app::{Effect, Intent, Page};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::metrics::{RequestLogPage, RequestLogRow, TimeWindow};
use crate::ui::hit::HitMap;
use crate::ui::text;
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

fn load_first_page(app: &mut App) {
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
        result: ObservabilityOutcome::Ready(RequestLogPage {
            rows: vec![
                RequestLogRow {
                    timestamp_us: Some(1_700_000_000_000_000),
                    method: Some("GET".to_owned()),
                    route: Some("/v1/agents/sessions".to_owned()),
                    status: Some(200),
                    duration_ms: Some(12),
                    session_id: Some("sess_a".to_owned()),
                },
                RequestLogRow {
                    timestamp_us: None,
                    method: None,
                    route: Some("/v1/agents/metrics".to_owned()),
                    status: Some(500),
                    duration_ms: None,
                    session_id: None,
                },
            ],
            page,
            has_next: true,
            window_start_us: 0,
            window_end_us: 1,
        }),
    });
}

#[test]
fn window_chips_and_the_next_button_are_clickable() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app);
    let (_terminal, hits) = draw(&mut app, 120, 30);

    let next = Intent::MetricsPageNext;
    let rect = hits.region_rect(next.clone()).expect("next page");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(next.clone()));
    app.apply(next);
    assert_eq!(app.request_log().page(), 1);

    // A window change invalidates the page request and restarts at page 0.
    let window = Intent::SetMetricsWindow(TimeWindow::TwentyFourHours);
    let rect = hits.region_rect(window.clone()).expect("24h chip");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(window.clone()));
    app.apply(window);
    assert_eq!(app.request_log().window(), TimeWindow::TwentyFourHours);
    assert_eq!(app.request_log().page(), 0);
}

#[test]
fn an_empty_page_after_paging_keeps_the_way_back() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app);
    let (generation, window, page) = match app.apply(Intent::MetricsPageNext).as_slice() {
        [Effect::LoadRequestLog {
            generation,
            window,
            page,
            ..
        }] => (*generation, *window, *page),
        _ => panic!("expected request-log page"),
    };
    app.handle_worker(WorkerEvent::RequestLogLoaded {
        epoch: app.epoch(),
        generation,
        window,
        page,
        result: ObservabilityOutcome::Ready(RequestLogPage {
            rows: Vec::new(),
            page,
            has_next: false,
            window_start_us: 0,
            window_end_us: 1,
        }),
    });
    let (terminal, hits) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains(text::EMPTY_REQUEST_LOG));
    assert!(hits.region_rect(Intent::MetricsPagePrev).is_some());
}

#[test]
fn narrow_layout_keeps_the_route_and_marks_missing_values() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app);
    let (terminal, _) = draw(&mut app, 60, 24);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::REQUEST_COL_ROUTE));
    assert!(text.contains("/v1/agents/sessions"));
    assert!(text.contains(text::DASH));
    assert!(text.contains(text::PAGE_NEXT));
}
