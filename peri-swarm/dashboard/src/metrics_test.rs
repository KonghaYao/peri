use serde_json::Value;

use super::*;
use crate::observability::{PromSample, PromSeries};
use crate::stub_http::{Stub, StubResponse};

fn series(labels: &[(&str, &str)], samples: &[(i64, f64)]) -> PromSeries {
    PromSeries {
        labels: labels
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        values: samples
            .iter()
            .map(|(timestamp, value)| PromSample {
                timestamp: *timestamp,
                value: *value,
            })
            .collect(),
    }
}

#[test]
fn windows_have_the_expected_span_and_prometheus_steps() {
    assert_eq!(TimeWindow::FifteenMinutes.seconds(), 900);
    assert_eq!(TimeWindow::OneHour.seconds(), 3_600);
    assert_eq!(TimeWindow::TwentyFourHours.seconds(), 86_400);
    assert_eq!(TimeWindow::FifteenMinutes.micros(), 900 * 1_000_000);
    assert_eq!(TimeWindow::FifteenMinutes.prom_steps(), (15, 15));
    assert_eq!(TimeWindow::OneHour.prom_steps(), (60, 60));
    assert_eq!(TimeWindow::TwentyFourHours.prom_steps(), (60, 300));
}

#[test]
fn positive_increments_ignore_counter_resets() {
    let delta = series_delta(&[
        PromSample {
            timestamp: 0,
            value: 5.0,
        },
        PromSample {
            timestamp: 10,
            value: 8.0,
        },
        PromSample {
            timestamp: 20,
            value: 2.0,
        },
        PromSample {
            timestamp: 30,
            value: 4.0,
        },
    ]);
    assert!((delta.total - 5.0).abs() < f64::EPSILON);
    assert_eq!(delta.first, Some(0));
    assert_eq!(delta.last, Some(30));
}

#[test]
fn histogram_quantile_interpolates_inside_a_bucket() {
    let quantile = histogram_quantile(0.95, &[(0.1, 50.0), (0.5, 100.0), (f64::INFINITY, 100.0)]);
    let value = quantile.expect("finite p95");
    assert!((value - 0.46).abs() < 1e-9, "{value}");
}

#[test]
fn histogram_quantile_in_the_open_bucket_is_unknown() {
    assert_eq!(
        histogram_quantile(0.95, &[(0.1, 5.0), (0.5, 8.0), (f64::INFINITY, 10.0)]),
        None
    );
}

#[test]
fn snapshot_reports_rates_coverage_and_p95() {
    let requests = [series(&[], &[(1_000, 100.0), (1_060, 160.0)])];
    let errors = [series(&[], &[(1_000, 6.0), (1_060, 12.0)])];
    let buckets = [
        series(&[("le", "0.1")], &[(1_000, 60.0), (1_060, 120.0)]),
        series(&[("le", "0.5")], &[(1_000, 70.0), (1_060, 130.0)]),
        series(&[("le", "inf")], &[(1_000, 70.0), (1_060, 130.0)]),
    ];
    let snapshot = build_snapshot(&requests, &errors, &buckets, 900_000_000, 1_100_000_000);
    assert_eq!(snapshot.total_requests, Some(60));
    let rate = snapshot.requests_per_minute.expect("rate");
    assert!((rate - 60.0).abs() < 1e-9, "{rate}");
    let error_rate = snapshot.error_rate.expect("error rate");
    assert!((error_rate - 0.1).abs() < 1e-9, "{error_rate}");
    assert!(snapshot.p95_seconds.is_some());
    assert_eq!(snapshot.covered_start_us, Some(1_000_000_000));
    assert_eq!(snapshot.covered_end_us, Some(1_060_000_000));
    assert!(!snapshot.is_empty());
}

#[test]
fn a_single_error_sample_does_not_become_a_zero_error_rate() {
    let requests = [series(&[], &[(1_000, 100.0), (1_060, 160.0)])];
    let errors = [series(&[], &[(1_060, 2.0)])];
    let snapshot = build_snapshot(&requests, &errors, &[], 900_000_000, 1_100_000_000);
    assert_eq!(snapshot.error_rate, None);
    assert_eq!(snapshot.total_requests, Some(60));
}

#[test]
fn missing_series_stays_empty() {
    let snapshot = build_snapshot(&[], &[], &[], 0, 1);
    assert!(snapshot.is_empty());
    assert_eq!(snapshot.requests_per_minute, None);
    assert_eq!(snapshot.error_rate, None);
    assert_eq!(snapshot.p95_seconds, None);
    assert_eq!(snapshot.total_requests, None);
}

#[test]
fn request_log_sql_is_bounded_and_paged() {
    let sql = request_log_sql(1_000, 2_000, 2);
    assert!(sql.contains("FROM \"peri_swarm_logs\""), "{sql}");
    assert!(sql.contains("_timestamp >= 1000"), "{sql}");
    assert!(sql.contains("_timestamp < 2000"), "{sql}");
    assert!(sql.contains("ORDER BY _timestamp DESC"), "{sql}");
    assert!(sql.contains("LIMIT 11"), "{sql}");
    assert!(sql.contains("OFFSET 20"), "{sql}");
}

#[test]
fn request_hits_keep_missing_fields_as_none() {
    let row = parse_row(&serde_json::json!({
        "_timestamp": 1_700_000_000_123_456_u64,
        "method": "GET",
        "status": 200,
        "duration_ms": 12,
    }));
    assert_eq!(row.timestamp_us, Some(1_700_000_000_123_456));
    assert_eq!(row.method.as_deref(), Some("GET"));
    assert_eq!(row.route, None);
    assert_eq!(row.status, Some(200));
    assert_eq!(row.duration_ms, Some(12));
    assert_eq!(row.session_id, None);
}

type MatrixSeries<'a> = (&'a [(&'a str, &'a str)], &'a [(i64, &'a str)]);

fn matrix(series: &[MatrixSeries<'_>]) -> String {
    let result: Vec<Value> = series
        .iter()
        .map(|(labels, values)| {
            serde_json::json!({
                "metric": labels
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
                "values": values
                    .iter()
                    .map(|(timestamp, value)| serde_json::json!([timestamp, value]))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "status": "success",
        "data": { "resultType": "matrix", "result": result },
    })
    .to_string()
}

#[tokio::test]
async fn load_core_metrics_queries_prometheus_and_computes_values() {
    let now_us = 1_800_000_000_000_000_u64;
    let end_s = (now_us / 1_000_000) as i64;
    let start_s = end_s - 900;
    let responses = vec![
        StubResponse::json(
            200,
            "OK",
            &matrix(&[(&[], &[(start_s, "10"), (end_s, "40")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &matrix(&[(&[], &[(start_s, "1"), (end_s, "5")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &matrix(&[
                (&[("le", "0.1")], &[(start_s, "20"), (end_s, "60")]),
                (&[("le", "0.5")], &[(start_s, "25"), (end_s, "65")]),
                (&[("le", "inf")], &[(start_s, "25"), (end_s, "65")]),
            ]),
        ),
    ];
    let stub = Stub::start(responses);
    let client =
        OpenObserveClient::new(&stub.base_url(), "default", "alice", "s3cret").expect("client");
    let snapshot = load_core_metrics(&client, TimeWindow::FifteenMinutes, now_us)
        .await
        .expect("metrics");
    assert_eq!(snapshot.total_requests, Some(30));
    assert_eq!(snapshot.error_rate, Some(4.0 / 30.0));
    assert!(snapshot.p95_seconds.is_some());

    let requests = stub.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path.split('?').next(),
            Some("/api/default/prometheus/api/v1/query_range")
        );
    }
    let first = &requests[0].path;
    assert!(first.contains("peri_swarm_http_server_requests"), "{first}");
    assert!(
        first.contains(&format!("start={}", start_s - 15)),
        "{first}"
    );
    assert!(first.contains(&format!("end={end_s}")), "{first}");
    assert!(first.contains("step=15"), "{first}");
}

#[tokio::test]
async fn load_request_log_uses_sql_and_detects_a_next_page() {
    let now_us = 1_800_000_000_000_000_u64;
    let hits: Vec<Value> = (0..11)
        .map(|index| {
            serde_json::json!({
                "_timestamp": 1_799_999_999_000_000_u64 - index,
                "method": "GET",
                "route": "/v1/agents/sessions",
                "status": 200,
                "duration_ms": 5
            })
        })
        .collect();
    let body = serde_json::json!({ "hits": hits, "total": 11 }).to_string();
    let stub = Stub::start(vec![StubResponse::json(200, "OK", &body)]);
    let client =
        OpenObserveClient::new(&stub.base_url(), "default", "alice", "s3cret").expect("client");
    let page = load_request_log(&client, TimeWindow::OneHour, 1, now_us)
        .await
        .expect("page");
    assert_eq!(page.rows.len(), REQUEST_LOG_PAGE_SIZE);
    assert!(page.has_next);
    assert_eq!(page.page, 1);

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0].body).expect("json");
    let sql = body["query"]["sql"].as_str().expect("sql");
    assert!(sql.contains("ORDER BY _timestamp DESC"), "{sql}");
    assert!(sql.contains("LIMIT 11"), "{sql}");
    assert!(sql.contains("OFFSET 10"), "{sql}");
    assert_eq!(body["query"]["from"], 0);
    assert_eq!(body["query"]["size"], 11);
}
