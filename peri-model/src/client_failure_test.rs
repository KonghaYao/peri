//! 模型 HTTP client 构造失败（如平台 TLS 根证书不可用）的跨 adapter 回归（#478）。
//!
//! 三个 adapter 的 constructor 保持无副作用，失败保留到首次 `prepare_stream`/`stream`
//! 以可展示的 `ModelError` 返回，不能让进程 unwind。

use tokio_util::sync::CancellationToken;

use crate::transport::http::build_client;
use crate::{
    AnthropicConfig, AnthropicModel, Model, ModelError, ModelRequest, ModelRuntimeConfig,
    OpenAiConfig, OpenAiModel, ResponsesConfig, ResponsesModel, TransportErrorKind,
};

/// 与平台 TLS 初始化失败走同一 `ClientBuilder::build` 失败分支的确定性构造。
fn client_construction_failure() -> ModelError {
    build_client(
        reqwest::Client::builder().tls_backend_preconfigured(()),
        &ModelRuntimeConfig::with_full_observation(),
        true,
    )
    .expect_err("builder 失败必须返回错误")
}

fn assert_displayable(error: &ModelError) {
    assert_eq!(error.transport_kind(), Some(TransportErrorKind::Tls));
    assert!(error.to_string().contains("tls"));
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.message(), Some("builder error"));
    assert!(
        diagnostic
            .causes()
            .iter()
            .any(|cause| cause.contains("Unknown TLS backend")),
        "错误必须保留 builder 原始诊断: {diagnostic:?}"
    );
}

fn anthropic_config() -> AnthropicConfig {
    AnthropicConfig::new(
        "https://api.anthropic.com".parse().expect("endpoint"),
        "test-key",
        "claude-sonnet-4-6",
    )
}

fn openai_config() -> OpenAiConfig {
    OpenAiConfig::new(
        "https://api.openai.com/v1".parse().expect("endpoint"),
        "test-key",
        "gpt-4o",
    )
}

fn responses_config() -> ResponsesConfig {
    ResponsesConfig::new(
        "https://api.openai.com/v1".parse().expect("endpoint"),
        "test-key",
        "gpt-5",
    )
}

#[test]
fn anthropic_prepare_stream_surfaces_client_construction_failure() {
    let model =
        AnthropicModel::with_client_failure(anthropic_config(), client_construction_failure());
    let error = model
        .prepare_stream(ModelRequest::default())
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}

#[tokio::test]
async fn anthropic_stream_surfaces_client_construction_failure() {
    let model =
        AnthropicModel::with_client_failure(anthropic_config(), client_construction_failure());
    let error = model
        .stream(ModelRequest::default(), CancellationToken::new())
        .await
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}

#[test]
fn openai_prepare_stream_surfaces_client_construction_failure() {
    let model = OpenAiModel::with_client_failure(openai_config(), client_construction_failure());
    let error = model
        .prepare_stream(ModelRequest::default())
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}

#[tokio::test]
async fn openai_stream_surfaces_client_construction_failure() {
    let model = OpenAiModel::with_client_failure(openai_config(), client_construction_failure());
    let error = model
        .stream(ModelRequest::default(), CancellationToken::new())
        .await
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}

#[test]
fn responses_prepare_stream_surfaces_client_construction_failure() {
    let model =
        ResponsesModel::with_client_failure(responses_config(), client_construction_failure());
    let error = model
        .prepare_stream(ModelRequest::default())
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}

#[tokio::test]
async fn responses_stream_surfaces_client_construction_failure() {
    let model =
        ResponsesModel::with_client_failure(responses_config(), client_construction_failure());
    let error = model
        .stream(ModelRequest::default(), CancellationToken::new())
        .await
        .expect_err("不得 panic，应返回错误");
    assert_displayable(&error);
}
