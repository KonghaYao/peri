use std::time::{Duration, Instant};

use super::*;
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
