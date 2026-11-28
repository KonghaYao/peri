use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;
use crate::app::{ActionId, Effect, FieldId, Focus, Intent, Page};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::metrics::TimeWindow;
use crate::observability::{ObservabilityError, SearchPage};
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

fn app_without_observability() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, "synthetic-key").expect("config");
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

fn set_sql(app: &mut App, sql: &str) {
    app.apply(Intent::SelectPage(Page::Query));
    app.apply(Intent::FocusTextArea(FieldId::QuerySql));
    app.handle_paste(sql);
}

fn run_query(app: &mut App) -> u64 {
    match app.apply(Intent::RunQuery).as_slice() {
        [Effect::RunQuery { generation, .. }] => *generation,
        _ => panic!("expected one RunQuery effect"),
    }
}

fn generation_of(effects: &[Effect]) -> u64 {
    match effects {
        [Effect::RunQuery { generation, .. }] => *generation,
        _ => panic!("expected one RunQuery effect"),
    }
}

fn finish(app: &mut App, generation: u64, hits: Vec<Value>, columns: Vec<String>) {
    let total = hits.len() as u64;
    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::Ready(SearchPage {
            hits,
            total,
            columns,
        }),
    });
}

fn rows(count: usize, columns: usize) -> Vec<Value> {
    (0..count)
        .map(|row| {
            let mut object = serde_json::Map::new();
            for column in 0..columns {
                object.insert(format!("col{column}"), json!(format!("r{row}c{column}")));
            }
            Value::Object(object)
        })
        .collect()
}

fn load_first_page(app: &mut App, hits: Vec<Value>, columns: Vec<String>) -> u64 {
    set_sql(app, "SELECT * FROM \"peri_swarm_logs\"");
    let generation = run_query(app);
    finish(app, generation, hits, columns);
    generation
}

#[test]
fn the_query_page_is_not_a_placeholder() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::Query));
    let (terminal, _hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);

    assert!(text.contains(text::QUERY_TITLE), "{text}");
    assert!(text.contains(text::QUERY_HINT), "{text}");
    assert!(!text.contains(text::SCOPE_EMPTY), "{text}");
}

#[test]
fn narrow_and_wide_frames_draw_a_long_editor_and_many_columns() {
    let (mut app, _dir) = app_with_observability();
    let sql = "SELECT col0, col1, col2, col3, col4, col5, col6, col7 FROM \"peri_swarm_logs\"\n"
        .repeat(40);
    set_sql(&mut app, &sql);
    let generation = run_query(&mut app);
    finish(&mut app, generation, rows(20, 8), Vec::new());

    for (width, height) in [(60, 36), (100, 40), (140, 48)] {
        let (terminal, _hits) = draw(&mut app, width, height);
        let text = buffer_text(&terminal);
        assert!(text.contains(text::QUERY_TITLE), "width {width}");
        assert!(text.contains("col0"), "width {width}");
    }
}

#[test]
fn response_columns_win_over_json_keys() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(
        &mut app,
        vec![json!({"aaa": "value-aaa", "zzz": "value-zzz"})],
        vec!["zzz".to_owned()],
    );
    let (terminal, _hits) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("zzz"), "{text}");
    assert!(text.contains("value-zzz"), "{text}");
    assert!(!text.contains("aaa"), "{text}");

    // Reload without reported columns: the JSON keys become the headers.
    let generation = run_query(&mut app);
    finish(
        &mut app,
        generation,
        vec![json!({"aaa": "value-aaa", "zzz": "value-zzz"})],
        Vec::new(),
    );
    let (terminal, _hits) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("aaa"), "{text}");
    assert!(text.contains("value-aaa"), "{text}");
}

#[test]
fn null_nested_and_long_values_render_safely() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(
        &mut app,
        vec![json!({
            "flag": true,
            "long": "x".repeat(200),
            "nested": {"k": [1, 2]},
            "nothing": null,
            "number": 3.5,
        })],
        Vec::new(),
    );
    let (terminal, _hits) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::DASH), "{text}");
    assert!(text.contains("{\"k\":[1,2]}"), "{text}");
    assert!(text.contains('…'), "{text}");
    assert!(text.contains("true"), "{text}");
    assert!(text.contains("3.5"), "{text}");
}

#[test]
fn cell_values_are_flattened_bounded_and_single_line() {
    assert_eq!(text::query_cell(None), text::DASH);
    assert_eq!(text::query_cell(Some(&Value::Null)), text::DASH);
    assert_eq!(text::query_cell(Some(&json!(true))), "true");
    assert_eq!(text::query_cell(Some(&json!(42))), "42");
    assert_eq!(
        text::query_cell(Some(&json!({"k": [1, 2]}))),
        "{\"k\":[1,2]}"
    );
    assert_eq!(text::query_cell(Some(&json!("a\nb\tc"))), "a b c");

    let long = "y".repeat(text::QUERY_CELL_MAX_CHARS + 30);
    let rendered = text::query_cell(Some(&json!(long)));
    assert!(rendered.ends_with('…'), "{rendered}");
    assert_eq!(rendered.chars().count(), text::QUERY_CELL_MAX_CHARS + 1);
}

#[test]
fn the_pager_and_column_notice_are_visible_without_scrolling() {
    let (mut app, _dir) = app_with_observability();
    // A full page of twenty rows used to push both controls below the fold.
    load_first_page(&mut app, rows(20, 8), Vec::new());
    for (width, height) in [(120, 40), (80, 24)] {
        let (terminal, hits) = draw(&mut app, width, height);
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Showing "),
            "column notice missing at {width}x{height}: {text}"
        );
        assert!(
            text.contains(text::PAGE_NEXT),
            "pager missing at {width}x{height}: {text}"
        );
        assert!(
            hits.region_rect(Intent::QueryPageNext).is_some(),
            "next page is off screen at {width}x{height}"
        );
    }
}

#[test]
fn a_limit_statement_explains_that_server_paging_is_off() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT * FROM \"peri_swarm_logs\" LIMIT 20");
    let generation = run_query(&mut app);
    finish(&mut app, generation, rows(20, 3), Vec::new());

    let (terminal, hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("SQL has LIMIT/OFFSET"), "{text}");
    assert!(text.contains("server-side paging is off"), "{text}");
    assert!(hits.region_rect(Intent::QueryPageNext).is_none(), "{text}");
    assert!(hits.region_rect(Intent::QueryPagePrev).is_none(), "{text}");
    // The table itself is still drawn for the first page.
    assert!(text.contains("col0"), "{text}");
}

#[test]
fn empty_results_show_the_honest_empty_state() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app, Vec::new(), Vec::new());
    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);

    assert!(text.contains(text::QUERY_EMPTY), "{text}");
    assert!(hits.region_rect(Intent::QueryPageNext).is_none());
    assert!(hits.region_rect(Intent::QueryPagePrev).is_none());
}

#[test]
fn an_error_replaces_the_previous_table() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app, rows(20, 3), Vec::new());
    assert_eq!(app.query().rows().len(), 20);

    let generation = run_query(&mut app);
    app.handle_worker(WorkerEvent::QueryLoaded {
        epoch: app.epoch(),
        generation,
        result: ObservabilityOutcome::Failed(ObservabilityError::Http {
            status: 500,
            detail: " Internal Server Error".to_owned(),
        }),
    });
    let (terminal, _hits) = draw(&mut app, 100, 40);
    let text = buffer_text(&terminal);

    assert!(text.contains("Could not run the query:"), "{text}");
    assert!(text.contains("500"), "{text}");
    assert!(!text.contains("col0"), "{text}");
}

#[test]
fn not_configured_offers_settings_and_no_table() {
    let (mut app, _dir) = app_without_observability();
    app.apply(Intent::SelectPage(Page::Query));
    assert!(app.metrics_due(std::time::Instant::now()).is_empty());
    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);

    assert!(
        text.contains(text::OBSERVABILITY_NOT_CONFIGURED_MESSAGE),
        "{text}"
    );
    assert!(hits
        .region_rect(Intent::Activate(ActionId::OpenSettings))
        .is_some());
    assert!(!text.contains(text::QUERY_EMPTY), "{text}");
}

#[test]
fn a_sql_limit_footer_stops_advertising_paging() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT * FROM \"peri_swarm_logs\" LIMIT 20");
    let generation = run_query(&mut app);
    finish(&mut app, generation, rows(20, 3), Vec::new());
    app.apply(Intent::FocusContent);
    assert!(app.query().manual_limit());

    let (terminal, _hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::FOOTER_QUERY_MANUAL_LIMIT), "{text}");
    assert!(!text.contains("PgUp/PgDn"), "{text}");

    // Narrow terminals still get the LIMIT note instead of the paging keys.
    let (terminal, _hits) = draw(&mut app, 79, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("SQL LIMIT/OFFSET"), "{text}");
    assert!(!text.contains("PgUp/PgDn"), "{text}");
}

#[test]
fn a_footer_without_a_sql_limit_keeps_advertising_paging() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app, rows(20, 3), Vec::new());
    app.apply(Intent::FocusContent);
    assert!(!app.query().manual_limit());

    let (terminal, _hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::FOOTER_QUERY), "{text}");
    assert!(text.contains("PgUp/PgDn page"), "{text}");
    assert!(!text.contains(text::FOOTER_QUERY_MANUAL_LIMIT), "{text}");
}

#[test]
fn pager_buttons_stop_at_the_row_cap() {
    let (mut app, _dir) = app_with_observability();
    load_first_page(&mut app, rows(20, 3), Vec::new());

    let (_terminal, hits) = draw(&mut app, 140, 48);
    assert!(hits.region_rect(Intent::QueryPageNext).is_some());
    assert!(hits.region_rect(Intent::QueryPagePrev).is_none());

    for expected_page in 1..=4 {
        let (_terminal, hits) = draw(&mut app, 140, 48);
        let next = Intent::QueryPageNext;
        let rect = hits.region_rect(next.clone()).expect("next page");
        assert_eq!(hits.click(false, rect.x, rect.y), Some(next.clone()));
        let effects = app.apply(next);
        let generation = generation_of(&effects);
        finish(&mut app, generation, rows(20, 3), Vec::new());
        assert_eq!(app.query().applied_page(), expected_page);
    }

    let (terminal, hits) = draw(&mut app, 140, 48);
    assert!(hits.region_rect(Intent::QueryPageNext).is_none());
    assert!(hits.region_rect(Intent::QueryPagePrev).is_some());
    assert!(buffer_text(&terminal).contains("Page 5"));
}

#[test]
fn window_chips_and_run_are_clickable() {
    let (mut app, _dir) = app_with_observability();
    set_sql(&mut app, "SELECT 1");
    let (_terminal, hits) = draw(&mut app, 120, 30);

    let window = Intent::SetMetricsWindow(TimeWindow::TwentyFourHours);
    let rect = hits.region_rect(window.clone()).expect("24h chip");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(window.clone()));
    app.apply(window);
    assert_eq!(app.query().window(), TimeWindow::TwentyFourHours);

    let run = Intent::Activate(ActionId::RunQuery);
    let rect = hits.region_rect(run.clone()).expect("run button");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(run.clone()));
    let effects = app.apply(run);
    assert!(generation_of(&effects) > 0);
}

#[test]
fn clicking_the_editor_focuses_the_sql_field() {
    let (mut app, _dir) = app_with_observability();
    app.apply(Intent::SelectPage(Page::Query));
    let (_terminal, hits) = draw(&mut app, 120, 30);
    let field = FieldId::QuerySql;
    let rect = hits
        .regions()
        .iter()
        .find(|region| {
            matches!(
                region.intent,
                Intent::FocusTextAreaAt { field: found, .. } if found == field
            )
        })
        .expect("editor region")
        .rect;

    let intent = hits
        .click(false, rect.x + 2, rect.y + 1)
        .expect("editor click");
    app.apply(intent);
    assert_eq!(app.focus(), Focus::Field(field));
    assert!(matches!(app.text_area(field), Some(area) if area.text.is_empty()));
}
