use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::app::{Effect, FieldId, Intent, Page};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::metrics::TimeWindow;
use crate::observability::{ObservabilityError, SearchPage};
use crate::query::{SqlError, QUERY_PAGE_SIZE};
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

fn app_without_observability() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, "synthetic-key").expect("config");
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
}

fn set_sql(app: &mut App, sql: &str) {
    app.apply(Intent::SelectPage(Page::Query));
    app.query.sql.text = sql.to_owned();
    app.query.sql.cursor = sql.chars().count();
}

fn query_request(effects: &[Effect]) -> (u64, String, u64, u64, u64, u64) {
    match effects {
        [Effect::RunQuery {
            generation,
            sql,
            start_us,
            end_us,
            from,
            size,
            ..
        }] => (*generation, sql.clone(), *start_us, *end_us, *from, *size),
        _ => panic!("expected exactly one RunQuery effect"),
    }
}

fn hits(count: usize) -> Vec<serde_json::Value> {
    (0..count).map(|index| json!({ "n": index })).collect()
}

fn finish(app: &mut App, generation: u64, hits: Vec<serde_json::Value>) {
    let total = hits.len() as u64;
    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::Ready(SearchPage {
            hits,
            total,
            columns: Vec::new(),
        }),
    });
}

#[test]
fn run_query_emits_one_effect_with_the_default_hour_window() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT * FROM \"peri_swarm_logs\"");

    let effects = app.apply(Intent::RunQuery);
    let (_, sql, start_us, end_us, from, size) = query_request(&effects);
    assert_eq!(sql, "SELECT * FROM \"peri_swarm_logs\"");
    assert_eq!(app.query().window(), TimeWindow::OneHour);
    assert_eq!(
        end_us.saturating_sub(start_us),
        TimeWindow::OneHour.micros()
    );
    assert!(end_us > 0);
    assert_eq!(from, 0);
    assert_eq!(size, QUERY_PAGE_SIZE as u64);
    assert!(app.query().loading());
    assert!(app.query().in_flight());
    assert!(app.query().validation_error().is_none());
}

#[test]
fn run_query_uses_the_selected_window() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    app.apply(Intent::SetMetricsWindow(TimeWindow::FifteenMinutes));

    let effects = app.apply(Intent::RunQuery);
    let (_, _, start_us, end_us, _, _) = query_request(&effects);
    assert_eq!(app.query().window(), TimeWindow::FifteenMinutes);
    assert_eq!(
        end_us.saturating_sub(start_us),
        TimeWindow::FifteenMinutes.micros()
    );
}

#[test]
fn invalid_statements_never_reach_the_worker() {
    let (mut app, _dir) = app_with_observability();

    app.apply(Intent::SelectPage(Page::Query));
    assert!(app.apply(Intent::RunQuery).is_empty());
    assert_eq!(
        app.query().validation_error(),
        Some(text::sql_error(SqlError::Empty))
    );

    set_sql(&mut app, "DELETE FROM logs");
    assert!(app.apply(Intent::RunQuery).is_empty());
    assert_eq!(
        app.query().validation_error(),
        Some(text::sql_error(SqlError::NotSelect))
    );
    assert!(!app.query().in_flight());
    assert!(app.query().error().is_none());
}

#[test]
fn a_successful_run_loads_columns_and_rows() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT a, b FROM logs");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));

    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::Ready(SearchPage {
            hits: vec![json!({"a": 1, "b": 2}), json!({"a": 3})],
            total: 2,
            columns: vec!["b".to_owned()],
        }),
    });

    assert!(!app.query().loading());
    assert!(!app.query().in_flight());
    assert_eq!(app.query().columns(), ["b"]);
    assert_eq!(app.query().rows().len(), 2);
    assert_eq!(app.query().applied_page(), 0);
    assert!(app.query().ever_loaded());
    assert!(app.query().error().is_none());
}

#[test]
fn a_failed_run_shows_a_safe_error_and_clears_the_table() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (first, ..) = query_request(&app.apply(Intent::RunQuery));
    finish(&mut app, first, hits(3));
    assert_eq!(app.query().rows().len(), 3);

    let (second, ..) = query_request(&app.apply(Intent::RunQuery));
    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation: second,
        result: ObservabilityOutcome::Failed(ObservabilityError::Http {
            status: 500,
            detail: " Internal Server Error".to_owned(),
        }),
    });

    assert!(!app.query().loading());
    assert!(app.query().rows().is_empty());
    assert!(!app.query().ever_loaded());
    let error = app.query().error().expect("error");
    assert!(error.starts_with("Could not run the query:"), "{error}");
    assert!(error.contains("500"), "{error}");
    assert!(!error.contains("pass"), "{error}");
}

#[test]
fn not_configured_never_sends_a_request() {
    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::Query));
    assert!(app.metrics_due(Instant::now()).is_empty());
    assert!(app.query().not_configured());

    app.query.sql.text = "SELECT 1".to_owned();
    assert!(app.apply(Intent::RunQuery).is_empty());
    assert!(app.query().not_configured());
    assert!(!app.query().in_flight());
}

#[test]
fn a_not_configured_result_from_the_worker_clears_the_page() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));
    finish(&mut app, generation, hits(2));
    assert_eq!(app.query().rows().len(), 2);

    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));
    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::NotConfigured,
    });

    assert!(app.query().not_configured());
    assert!(app.query().rows().is_empty());
    assert!(!app.query().ever_loaded());
    assert!(!app.query().loading());
    assert!(app.query().error().is_none());
}

#[test]
fn a_server_returning_too_many_rows_is_cut_to_one_page() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));
    finish(&mut app, generation, hits(QUERY_PAGE_SIZE + 5));

    assert_eq!(app.query().rows().len(), QUERY_PAGE_SIZE);
    assert!(app.query().can_next());
}

#[test]
fn a_window_change_invalidates_the_in_flight_generation() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));

    app.apply(Intent::SetMetricsWindow(TimeWindow::TwentyFourHours));
    assert_eq!(app.query().window(), TimeWindow::TwentyFourHours);
    assert!(!app.query().loading());
    assert!(!app.query().in_flight());

    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::Ready(SearchPage {
            hits: hits(2),
            total: 2,
            columns: Vec::new(),
        }),
    });
    assert!(app.query().rows().is_empty());
    assert!(!app.query().ever_loaded());
    assert!(app.query().error().is_none());
}

#[test]
fn a_stale_epoch_is_discarded() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));

    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch() + 1,
        generation,
        result: ObservabilityOutcome::Ready(SearchPage {
            hits: hits(2),
            total: 2,
            columns: Vec::new(),
        }),
    });
    assert!(app.query().rows().is_empty());
    assert!(app.query().in_flight());
}

#[test]
fn paging_stops_at_the_row_cap() {
    let (mut app, _dir) = app_with_observability();
    assert!(app.apply(Intent::QueryPageNext).is_empty());
    assert!(app.apply(Intent::QueryPagePrev).is_empty());

    set_sql(&mut app, "SELECT 1");
    let (generation, _, _, _, from, _) = query_request(&app.apply(Intent::RunQuery));
    assert_eq!(from, 0);
    finish(&mut app, generation, hits(QUERY_PAGE_SIZE));
    assert!(app.query().can_next());

    let mut offsets = Vec::new();
    for _ in 0..4 {
        let (generation, _, _, _, from, size) = query_request(&app.apply(Intent::QueryPageNext));
        assert_eq!(size, QUERY_PAGE_SIZE as u64);
        offsets.push(from);
        finish(&mut app, generation, hits(QUERY_PAGE_SIZE));
    }
    assert_eq!(offsets, [20, 40, 60, 80]);
    assert_eq!(app.query().applied_page(), 4);
    assert!(!app.query().can_next());
    assert!(app.apply(Intent::QueryPageNext).is_empty());

    // Prev from the last page reuses the executed statement.
    let (_, sql, _, _, from, _) = query_request(&app.apply(Intent::QueryPagePrev));
    assert_eq!(sql, "SELECT 1");
    assert_eq!(from, 60);
    assert_eq!(app.query().page(), 3);
}

#[test]
fn a_short_page_disables_next() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));
    finish(&mut app, generation, hits(QUERY_PAGE_SIZE - 1));

    assert!(!app.query().can_next());
    assert!(app.apply(Intent::QueryPageNext).is_empty());
}

#[test]
fn a_new_run_always_starts_at_the_first_page() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (generation, ..) = query_request(&app.apply(Intent::RunQuery));
    finish(&mut app, generation, hits(QUERY_PAGE_SIZE));

    let (generation, ..) = query_request(&app.apply(Intent::QueryPageNext));
    finish(&mut app, generation, hits(QUERY_PAGE_SIZE));
    assert_eq!(app.query().applied_page(), 1);

    let (_, sql, _, _, from, _) = query_request(&app.apply(Intent::RunQuery));
    assert_eq!(sql, "SELECT 1");
    assert_eq!(from, 0);
    assert_eq!(app.query().page(), 0);
}

#[test]
fn editing_the_statement_clears_the_validation_message() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::Query));
    app.apply(Intent::RunQuery);
    assert!(app.query().validation_error().is_some());

    app.apply(Intent::FocusTextArea(FieldId::QuerySql));
    app.handle_paste("SELECT 1");
    assert!(app.query().validation_error().is_none());
}

#[test]
fn ctrl_enter_runs_from_the_editor() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    app.apply(Intent::FocusTextArea(FieldId::QuerySql));

    let effects = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    let (_, sql, _, _, _, _) = query_request(&effects);
    assert_eq!(sql, "SELECT 1");
}

#[test]
fn run_is_ignored_outside_the_query_page() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::SessionLog));
    app.query.sql.text = "SELECT 1".to_owned();

    assert!(app.apply(Intent::RunQuery).is_empty());
    assert!(!app.query().in_flight());
}

#[test]
fn a_configured_connection_keeps_the_page_synced() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::Query));
    assert!(app.metrics_due(Instant::now()).is_empty());
    assert!(!app.query().not_configured());
}
