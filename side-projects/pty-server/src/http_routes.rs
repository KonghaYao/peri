use axum::{
    http::{header, HeaderValue},
    response::IntoResponse,
};

/// GET / 返回嵌入的 index.html。
pub async fn index() -> impl IntoResponse {
    let html: &'static str = include_str!("../index.html");
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        html,
    )
}
