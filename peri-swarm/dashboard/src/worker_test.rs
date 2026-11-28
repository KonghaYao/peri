use std::time::{Duration, Instant};

use super::*;
use crate::config::ObservabilityConfig;
use crate::metrics::TimeWindow;
use crate::stub_http::{Stub, StubResponse};

fn wait_event(worker: &mut WorkerHandle, timeout: Duration) -> Option<WorkerEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(event) = worker.try_event() {
            return Some(event);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn test_command(stub: &Stub) -> Command {
    Command::TestObservability {
        epoch: 7,
        base_url: stub.base_url(),
        org: "default".to_owned(),
        user: "alice".to_owned(),
        password: "s3cret".to_owned(),
    }
}

fn config_with_observability(stub: &Stub) -> Config {
    let observability =
        ObservabilityConfig::new(&stub.base_url(), "default", "alice", "s3cret").expect("config");
    Config::new("http://127.0.0.1:9", "synthetic-key")
        .expect("config")
        .with_observability(observability)
}

fn prom_matrix(series: &[(&str, &[(i64, &str)])]) -> String {
    let result: Vec<serde_json::Value> = series
        .iter()
        .map(|(le, values)| {
            let metric = if le.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::json!({ "le": le })
            };
            serde_json::json!({
                "metric": metric,
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

#[test]
fn test_observability_command_probes_and_returns_streams() {
    let stub = Stub::start(vec![StubResponse::json(200, "OK", r#"{"list":[{},{}]}"#)]);
    let mut worker = WorkerHandle::spawn(None);
    worker.send(test_command(&stub));

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::ObservabilityTested { epoch, result } => {
            assert_eq!(epoch, 7);
            let check = result.expect("probe succeeds");
            assert_eq!(check.streams, Some(2));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/default/streams");
}

#[test]
fn test_observability_command_reports_safe_errors() {
    let stub = Stub::start(vec![StubResponse::json(
        401,
        "Unauthorized",
        r#"{"message":"bad password s3cret"}"#,
    )]);
    let mut worker = WorkerHandle::spawn(None);
    worker.send(test_command(&stub));

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::ObservabilityTested { epoch, result } => {
            assert_eq!(epoch, 7);
            let error = result.expect_err("401 fails");
            let rendered = format!("{error} {error:?}");
            assert!(rendered.contains("401"), "{rendered}");
            assert!(!rendered.contains("s3cret"), "{rendered}");
            assert!(!rendered.contains("Authorization"), "{rendered}");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn load_core_metrics_returns_a_ready_snapshot() {
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let responses = vec![
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(now_s - 600, "10"), (now_s, "40")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(now_s - 600, "1"), (now_s, "3")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[
                ("0.1", &[(now_s - 600, "20"), (now_s, "60")]),
                ("0.5", &[(now_s - 600, "25"), (now_s, "65")]),
                ("inf", &[(now_s - 600, "25"), (now_s, "65")]),
            ]),
        ),
    ];
    let stub = Stub::start(responses);
    let mut worker = WorkerHandle::spawn(Some(config_with_observability(&stub)));
    worker.send(Command::LoadCoreMetrics {
        epoch: 7,
        generation: 3,
        window: TimeWindow::FifteenMinutes,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::CoreMetricsLoaded {
            epoch,
            generation,
            window,
            result,
        } => {
            assert_eq!(
                (epoch, generation, window),
                (7, 3, TimeWindow::FifteenMinutes)
            );
            match result {
                ObservabilityOutcome::Ready(snapshot) => {
                    assert_eq!(snapshot.total_requests, Some(30));
                    assert!(snapshot.requests_per_minute.is_some());
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(stub.requests().len(), 3);
}

#[test]
fn load_core_metrics_reports_not_configured_without_a_request() {
    let stub = Stub::start(Vec::new());
    let config = Config::new(stub.base_url().as_str(), "synthetic-key").expect("config");
    let mut worker = WorkerHandle::spawn(Some(config));
    worker.send(Command::LoadCoreMetrics {
        epoch: 1,
        generation: 2,
        window: TimeWindow::OneHour,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::CoreMetricsLoaded {
            epoch,
            generation,
            window,
            result,
        } => {
            assert_eq!((epoch, generation, window), (1, 2, TimeWindow::OneHour));
            assert!(matches!(result, ObservabilityOutcome::NotConfigured));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(stub.requests().is_empty());
}

#[test]
fn load_core_metrics_errors_are_scrubbed() {
    let stub = Stub::start(vec![StubResponse::json(
        500,
        "Internal Server Error",
        r#"{"message":"s3cret leaked"}"#,
    )]);
    let mut worker = WorkerHandle::spawn(Some(config_with_observability(&stub)));
    worker.send(Command::LoadCoreMetrics {
        epoch: 1,
        generation: 1,
        window: TimeWindow::FifteenMinutes,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::CoreMetricsLoaded { result, .. } => match result {
            ObservabilityOutcome::Failed(error) => {
                let rendered = format!("{error} {error:?}");
                assert!(!rendered.contains("s3cret"), "{rendered}");
                assert!(rendered.contains("500"), "{rendered}");
            }
            other => panic!("unexpected result: {other:?}"),
        },
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn load_agent_metrics_uses_the_gateway_and_does_not_need_openobserve() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"start_time":1,"end_time":2,"sessions":[{"session_id":"sess_1","agent_id":"root","turns":2,"calls":3,"last_active_at":"2026-01-01T00:00:00Z"}],"totals":{"turns":2,"calls":3},"truncated":true}"#,
    )]);
    let config = Config::new(&stub.base_url(), "synthetic-key").expect("config");
    let mut worker = WorkerHandle::spawn(Some(config));
    worker.send(Command::LoadAgentMetrics {
        epoch: 4,
        generation: 5,
        window: TimeWindow::TwentyFourHours,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::AgentMetricsLoaded {
            epoch,
            generation,
            window,
            result,
        } => {
            assert_eq!(
                (epoch, generation, window),
                (4, 5, TimeWindow::TwentyFourHours)
            );
            let metrics = result.expect("agent metrics");
            assert_eq!(metrics.sessions.len(), 1);
            assert_eq!(metrics.sessions[0].input_tokens, None);
            assert!(metrics.truncated);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .path
        .starts_with("/v1/agents/metrics?start_time="));
    assert!(requests[0].path.contains("&limit=500"));
}

#[test]
fn load_request_log_returns_rows_and_detects_the_next_page() {
    let hits: Vec<serde_json::Value> = (0..11)
        .map(|index| {
            serde_json::json!({
                "_timestamp": 1_799_999_999_000_000_u64 - index,
                "method": "GET",
                "route": "/v1/agents/sessions",
                "status": 200,
                "duration_ms": 7
            })
        })
        .collect();
    let body = serde_json::json!({ "hits": hits, "total": 11 }).to_string();
    let stub = Stub::start(vec![StubResponse::json(200, "OK", &body)]);
    let mut worker = WorkerHandle::spawn(Some(config_with_observability(&stub)));
    worker.send(Command::LoadRequestLog {
        epoch: 8,
        generation: 9,
        window: TimeWindow::OneHour,
        page: 2,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::RequestLogLoaded {
            epoch,
            generation,
            window,
            page,
            result,
        } => {
            assert_eq!(
                (epoch, generation, window, page),
                (8, 9, TimeWindow::OneHour, 2)
            );
            match result {
                ObservabilityOutcome::Ready(page) => {
                    assert_eq!(page.rows.len(), 10);
                    assert!(page.has_next);
                    assert_eq!(page.page, 2);
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn load_request_log_reports_not_configured() {
    let stub = Stub::start(Vec::new());
    let config = Config::new(&stub.base_url(), "synthetic-key").expect("config");
    let mut worker = WorkerHandle::spawn(Some(config));
    worker.send(Command::LoadRequestLog {
        epoch: 1,
        generation: 1,
        window: TimeWindow::FifteenMinutes,
        page: 0,
    });
    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::RequestLogLoaded { result, .. } => {
            assert!(matches!(result, ObservabilityOutcome::NotConfigured));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(stub.requests().is_empty());
}

#[test]
fn adopt_observability_enables_queries_without_a_restart() {
    let oo = Stub::start(vec![
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(1_000, "1"), (1_060, "2")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(1_000, "0"), (1_060, "0")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[
                ("0.1", &[(1_000, "1"), (1_060, "2")]),
                ("inf", &[(1_000, "1"), (1_060, "2")]),
            ]),
        ),
    ]);
    let config = Config::new("http://127.0.0.1:9", "synthetic-key").expect("config");
    let mut worker = WorkerHandle::spawn(Some(config));
    worker.send(Command::AdoptObservability {
        base_url: oo.base_url(),
        org: "default".to_owned(),
        user: "alice".to_owned(),
        password: "s3cret".to_owned(),
    });
    worker.send(Command::LoadCoreMetrics {
        epoch: 1,
        generation: 1,
        window: TimeWindow::FifteenMinutes,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::CoreMetricsLoaded { result, .. } => {
            assert!(matches!(result, ObservabilityOutcome::Ready(_)));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(oo.requests().len(), 3);
}

#[test]
fn adopt_keeps_the_observability_section() {
    let oo = Stub::start(vec![
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(1_000, "1"), (1_060, "2")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[("", &[(1_000, "0"), (1_060, "0")])]),
        ),
        StubResponse::json(
            200,
            "OK",
            &prom_matrix(&[
                ("0.1", &[(1_000, "1"), (1_060, "2")]),
                ("inf", &[(1_000, "1"), (1_060, "2")]),
            ]),
        ),
    ]);
    let mut worker = WorkerHandle::spawn(Some(config_with_observability(&oo)));
    worker.send(Command::Adopt {
        base_url: "http://127.0.0.1:9".to_owned(),
        api_key: "replacement-key".to_owned(),
    });
    worker.send(Command::LoadCoreMetrics {
        epoch: 2,
        generation: 1,
        window: TimeWindow::FifteenMinutes,
    });

    match wait_event(&mut worker, Duration::from_secs(5)).expect("event") {
        WorkerEvent::CoreMetricsLoaded { result, .. } => {
            assert!(matches!(result, ObservabilityOutcome::Ready(_)));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(oo.requests().len(), 3);
}
