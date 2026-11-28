use super::*;
use crate::stub_http::{Stub, StubResponse};

const BASIC_AUTH: &str = "Basic YWxpY2U6czNjcmV0";

fn client(base_url: &str) -> OpenObserveClient {
    OpenObserveClient::new(base_url, "myorg", "alice", "s3cret").expect("client")
}

#[tokio::test]
async fn search_sends_basic_auth_and_parses_hits() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"total":2,"hits":[{"_timestamp":10},{"_timestamp":20}]}"#,
    )]);
    let page = client(&stub.base_url())
        .search("SELECT * FROM \"peri_swarm_logs\"", 1, 2, 3, 4)
        .await
        .expect("search");
    assert_eq!(page.total, 2);
    assert_eq!(page.hits.len(), 2);

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/myorg/_search");
    assert_eq!(request.header("authorization"), Some(BASIC_AUTH));
    let body: Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(body["query"]["sql"], "SELECT * FROM \"peri_swarm_logs\"");
    assert_eq!(body["query"]["start_time"], 1);
    assert_eq!(body["query"]["end_time"], 2);
    assert_eq!(body["query"]["from"], 3);
    assert_eq!(body["query"]["size"], 4);
}

#[tokio::test]
async fn test_connection_uses_the_streams_endpoint() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"list":[{"name":"a"},{"name":"b"}]}"#,
    )]);
    let streams = client(&stub.base_url())
        .test_connection()
        .await
        .expect("probe");
    assert_eq!(streams, Some(2));

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/myorg/streams");
    assert_eq!(requests[0].header("authorization"), Some(BASIC_AUTH));
}

#[tokio::test]
async fn redirect_responses_are_not_followed() {
    let stub = Stub::start(vec![
        StubResponse::redirect("http://127.0.0.1:9/elsewhere"),
        StubResponse::json(200, "OK", r#"{"list":[]}"#),
    ]);
    let error = client(&stub.base_url())
        .test_connection()
        .await
        .expect_err("redirect must be reported");
    match error {
        ObservabilityError::Http { status, .. } => assert_eq!(status, 302),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(stub.requests().len(), 1, "redirect must not be followed");
}

#[tokio::test]
async fn http_errors_are_scrubbed() {
    // The server echoes the client's own password; it must not survive.
    let body = r#"{"message":"bad password s3cret"}"#.to_owned();
    let stub = Stub::start(vec![StubResponse::json(401, "Unauthorized", &body)]);
    let error = client(&stub.base_url())
        .test_connection()
        .await
        .expect_err("401 must fail");
    let rendered = format!("{error} {error:?}");
    assert!(rendered.contains("401"), "{rendered}");
    assert!(rendered.contains("Unauthorized"), "{rendered}");
    assert!(!rendered.contains("s3cret"), "{rendered}");
    assert!(!rendered.contains("Authorization"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

#[tokio::test]
async fn malformed_success_body_is_reported() {
    let stub = Stub::start(vec![StubResponse::json(200, "OK", "not json")]);
    assert_eq!(
        client(&stub.base_url())
            .search("SELECT 1", 0, 1, 0, 1)
            .await,
        Err(ObservabilityError::Malformed)
    );
}

#[tokio::test]
async fn query_range_uses_the_prometheus_endpoint_and_parses_values() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"__name__":"peri_swarm_http_server_requests","status":"200"},"values":[[100,"3"],[115,"7"]]}]}}"#,
    )]);
    let matrix = client(&stub.base_url())
        .query_range("sum(peri_swarm_http_server_requests)", 100, 200, 15)
        .await
        .expect("matrix");
    assert_eq!(matrix.series.len(), 1);
    assert_eq!(
        matrix.series[0].labels.get("status").map(String::as_str),
        Some("200")
    );
    assert_eq!(matrix.series[0].values.len(), 2);
    assert_eq!(matrix.series[0].values[1].timestamp, 115);
    assert!((matrix.series[0].values[1].value - 7.0).abs() < f64::EPSILON);

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path.split('?').next(),
        Some("/api/myorg/prometheus/api/v1/query_range")
    );
    assert_eq!(request.header("authorization"), Some(BASIC_AUTH));
    assert!(request.path.contains("query=sum"), "{}", request.path);
    assert!(request.path.contains("start=100"), "{}", request.path);
    assert!(request.path.contains("end=200"), "{}", request.path);
    assert!(request.path.contains("step=15"), "{}", request.path);
}

#[tokio::test]
async fn query_range_redirects_are_not_followed() {
    let stub = Stub::start(vec![
        StubResponse::redirect("http://127.0.0.1:9/elsewhere"),
        StubResponse::json(
            200,
            "OK",
            r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#,
        ),
    ]);
    let error = client(&stub.base_url())
        .query_range("up", 1, 2, 1)
        .await
        .expect_err("redirect must be reported");
    match error {
        ObservabilityError::Http { status, .. } => assert_eq!(status, 302),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(stub.requests().len(), 1, "redirect must not be followed");
}

#[tokio::test]
async fn query_range_errors_are_scrubbed() {
    let stub = Stub::start(vec![StubResponse::json(
        500,
        "Internal Server Error",
        r#"{"message":"Authorization: Basic YWxpY2U6czNjcmV0 failed"}"#,
    )]);
    let error = client(&stub.base_url())
        .query_range("up", 1, 2, 1)
        .await
        .expect_err("500 must fail");
    let rendered = format!("{error} {error:?}");
    assert!(rendered.contains("500"), "{rendered}");
    assert!(!rendered.contains("s3cret"), "{rendered}");
    assert!(!rendered.contains("YWxpY2U6czNjcmV0"), "{rendered}");
}

#[tokio::test]
async fn query_range_rejects_a_non_success_envelope() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"status":"error","error":"bad query"}"#,
    )]);
    assert_eq!(
        client(&stub.base_url()).query_range("up", 1, 2, 1).await,
        Err(ObservabilityError::Malformed)
    );
}

#[tokio::test]
async fn query_range_accepts_an_empty_matrix() {
    let stub = Stub::start(vec![StubResponse::json(
        200,
        "OK",
        r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#,
    )]);
    let matrix = client(&stub.base_url())
        .query_range("up", 1, 2, 1)
        .await
        .expect("empty");
    assert!(matrix.series.is_empty());
}

#[test]
fn debug_redacts_the_password() {
    let client = client("http://127.0.0.1:5080");
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("s3cret"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(rendered.contains("myorg"), "{rendered}");
}

#[test]
fn invalid_values_are_rejected_before_any_request() {
    assert!(matches!(
        OpenObserveClient::new("ftp://127.0.0.1:5080", "org", "user", "pw"),
        Err(ObservabilityError::InvalidConfig(
            ObservabilityConfigError::UnsupportedScheme
        ))
    ));
}
