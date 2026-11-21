use axum::body::to_bytes;
use axum::http::StatusCode;

use super::*;

async fn response_parts(error: GatewayError) -> (StatusCode, String) {
    let response = error.into_response();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn unknown_token_is_404() {
    let (status, body) = response_parts(GatewayError::UnknownToken).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "not found");
}

#[tokio::test]
async fn bad_request_is_400_without_detail() {
    let (status, body) =
        response_parts(GatewayError::BadRequest("missing upgrade header".into())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "bad request");
}

#[tokio::test]
async fn internal_error_is_500_without_detail() {
    let (status, body) = response_parts(GatewayError::Internal("cdp exploded".into())).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "internal error");
}
