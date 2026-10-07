#[cfg(target_os = "emscripten")]
use error::{ModelError, ModelResult, TransportErrorKind};

#[cfg(target_os = "emscripten")]
#[path = "../runtime/error.rs"]
mod error;

#[cfg(target_os = "emscripten")]
#[path = "cloudflare.rs"]
mod cloudflare;
#[cfg(target_os = "emscripten")]
#[path = "http.rs"]
mod http;

fn main() {}

#[cfg(target_os = "emscripten")]
mod contracts {
    use std::time::Duration;

    use futures::StreamExt;
    use tokio_util::sync::CancellationToken;
    use wasm_bindgen::prelude::*;

    use super::{cloudflare, http::HttpRequest, ModelError, TransportErrorKind};

    #[wasm_bindgen(inline_js = r#"
let original;
let state;
export function installFetchFixture(mode) {
    original ??= globalThis.fetch;
    state = { calls: 0, aborts: 0, pulls: 0, cancelled: 0, mode };
    globalThis.fetch = (url, options) => {
        state.calls++;
        state.url = url;
        state.method = options.method;
        state.headers = Object.fromEntries(new Headers(options.headers));
        state.body = options.body ? new TextDecoder().decode(options.body) : null;
        return new Promise((resolve, reject) => {
            let streamController;
            options.signal.addEventListener('abort', () => {
                state.aborts++;
                if (streamController) {
                    try { streamController.close(); } catch {}
                }
                reject(new DOMException('Fixture request aborted', 'AbortError'));
            }, {once: true});
            if (mode === 'headers-pending') return;
            if (mode === 'connect-error') {
                reject(new TypeError('Fixture connection failed', {cause: new Error('Underlying fixture cause')}));
                return;
            }
            const stream = new ReadableStream({
                start(controller) { streamController = controller; },
                pull(controller) {
                    state.pulls++;
                    if (mode === 'body-pending') return new Promise(() => {});
                    if (mode === 'infinite') { controller.enqueue(new Uint8Array(65536)); return; }
                    if (mode === 'oversized') {
                        controller.enqueue(new Uint8Array(1048577)); controller.close(); return;
                    }
                    if (mode === 'body-error' && state.pulls > 1) {
                        controller.error(new Error('Fixture body failed', {cause: new Error('Reader fixture cause')}));
                        return;
                    }
                    if (state.pulls === 1) controller.enqueue(new TextEncoder().encode('first'));
                    else if (state.pulls === 2) controller.enqueue(new TextEncoder().encode(' second'));
                    else controller.close();
                },
                cancel() { state.cancelled++; },
            });
            state.stream = stream;
            resolve(new Response(stream, {status: mode === 'status' ? 429 : 200,
                headers: {'x-request-id': 'fixture-request-id', 'content-type': 'text/plain'}}));
        });
    };
}
export function fetchFixtureState() {
    return JSON.stringify({...state, stream: undefined, locked: state.stream?.locked ?? false});
}
export function restoreFetchFixture() { globalThis.fetch = original; }
"#)]
    extern "C" {
        #[wasm_bindgen(js_name = installFetchFixture)]
        fn install(mode: &str);
        #[wasm_bindgen(js_name = fetchFixtureState)]
        fn fixture_state() -> String;
        #[wasm_bindgen(js_name = restoreFetchFixture)]
        fn restore();
    }

    struct Fixture;

    impl Fixture {
        fn new(mode: &str) -> Self {
            install(mode);
            Self
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            restore();
        }
    }

    fn state() -> serde_json::Value {
        serde_json::from_str(&fixture_state()).expect("fixture state JSON")
    }

    fn request(timeout: Option<Duration>) -> HttpRequest {
        let mut request = reqwest::Client::new()
            .post("https://fixture.test/model")
            .header("authorization", "Bearer fixture-only")
            .body("fixture request")
            .build()
            .expect("fixture request");
        *request.timeout_mut() = timeout;
        HttpRequest::new(request)
    }

    async fn wait_for_abort() {
        for _attempt in 0..100 {
            let snapshot = state();
            if snapshot["aborts"] == 1 && snapshot["locked"] == false {
                return;
            }
            peri_time::sleep(Duration::from_millis(2)).await;
        }
        panic!("fetch abort must release the owned reader: {}", state());
    }

    fn assert_send<T: Send>(_: &T) {}

    #[wasm_bindgen(js_name = runNativeFetchContracts, experimental_tokio)]
    pub async fn run() -> Result<String, JsValue> {
        let mut passed = Vec::new();
        {
            let _fixture = Fixture::new("status");
            let send = cloudflare::send(request(None), CancellationToken::new());
            assert_send(&send);
            let mut response = send.await.expect("native fetch response");
            assert_eq!(response.status, 429);
            assert_eq!(response.request_id.as_deref(), Some("fixture-request-id"));
            let mut body = Vec::new();
            while let Some(chunk) = response.body.next().await {
                body.extend(chunk.expect("stream chunk"));
            }
            assert_eq!(body, b"first second");
            assert_eq!(state()["headers"]["authorization"], "Bearer fixture-only");
            assert_eq!(state()["body"], "fixture request");
            assert_eq!(state()["method"], "POST");
            assert_eq!(state()["aborts"], 0);
            assert_eq!(state()["locked"], false);
            passed.push("status-headers-body-send-stream-eof");
        }
        {
            let _fixture = Fixture::new("connect-error");
            let error = cloudflare::send(request(None), CancellationToken::new())
                .await
                .err()
                .expect("connection failure");
            assert_eq!(error.transport_kind(), Some(TransportErrorKind::Connection));
            assert_eq!(
                error.diagnostic().message(),
                Some("Fixture connection failed")
            );
            assert!(error
                .diagnostic()
                .causes()
                .iter()
                .any(|cause| cause.contains("Underlying fixture cause")));
            passed.push("connection-error-cause");
        }
        {
            let _fixture = Fixture::new("headers-pending");
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let error = cloudflare::send(request(None), cancellation)
                .await
                .err()
                .expect("pre-cancelled send");
            assert!(error.is_cancelled());
            assert_eq!(state()["calls"], 0);
            passed.push("pre-cancelled-no-fetch");
        }
        {
            let _fixture = Fixture::new("headers-pending");
            let cancellation = CancellationToken::new();
            let mut send = Box::pin(cloudflare::send(request(None), cancellation.clone()));
            assert!(futures::poll!(&mut send).is_pending());
            peri_time::sleep(Duration::from_millis(2)).await;
            cancellation.cancel();
            let error = send.await.err().expect("cancelled headers");
            assert!(error.is_cancelled());
            wait_for_abort().await;
            passed.push("cancel-pending-headers");
        }
        {
            let _fixture = Fixture::new("headers-pending");
            let mut send = Box::pin(cloudflare::send(request(None), CancellationToken::new()));
            assert!(futures::poll!(&mut send).is_pending());
            peri_time::sleep(Duration::from_millis(2)).await;
            drop(send);
            wait_for_abort().await;
            passed.push("drop-pending-headers");
        }
        {
            let _fixture = Fixture::new("body-pending");
            let cancellation = CancellationToken::new();
            let mut response = cloudflare::send(request(None), cancellation.clone())
                .await
                .expect("body response");
            cancellation.cancel();
            assert!(response
                .body
                .next()
                .await
                .expect("cancel result")
                .unwrap_err()
                .is_cancelled());
            wait_for_abort().await;
            passed.push("cancel-pending-reader-release");
        }
        {
            let _fixture = Fixture::new("infinite");
            let response = cloudflare::send(request(None), CancellationToken::new())
                .await
                .expect("bounded body");
            peri_time::sleep(Duration::from_millis(2)).await;
            let pulls = state()["pulls"].as_u64().expect("pull count");
            assert!(
                pulls <= 6,
                "native reader must stop on bounded queue, pulls={pulls}"
            );
            peri_time::sleep(Duration::from_millis(2)).await;
            assert_eq!(state()["pulls"], pulls);
            drop(response);
            wait_for_abort().await;
            passed.push("bounded-body-queue-drop-abort");
        }
        for mode in ["headers-pending", "body-pending"] {
            let _fixture = Fixture::new(mode);
            let result = cloudflare::send(
                request(Some(Duration::from_millis(10))),
                CancellationToken::new(),
            )
            .await;
            let error = match result {
                Err(error) => error,
                Ok(mut response) => response
                    .body
                    .next()
                    .await
                    .expect("timeout item")
                    .expect_err("body timeout"),
            };
            assert_eq!(error.transport_kind(), Some(TransportErrorKind::Timeout));
            wait_for_abort().await;
            passed.push(if mode == "headers-pending" {
                "headers-timeout"
            } else {
                "body-timeout"
            });
        }
        {
            let _fixture = Fixture::new("oversized");
            let mut response = cloudflare::send(request(None), CancellationToken::new())
                .await
                .expect("oversized response");
            let error = response
                .body
                .next()
                .await
                .expect("limit error")
                .expect_err("source bound");
            assert!(error
                .diagnostic()
                .message()
                .unwrap_or_default()
                .contains("1 MiB"));
            wait_for_abort().await;
            passed.push("source-chunk-bound");
        }
        {
            let _fixture = Fixture::new("body-error");
            let mut response = cloudflare::send(request(None), CancellationToken::new())
                .await
                .expect("body error response");
            let mut failure: Option<ModelError> = None;
            while let Some(chunk) = response.body.next().await {
                if let Err(error) = chunk {
                    failure = Some(error);
                    break;
                }
            }
            let failure = failure.expect("reader failure");
            assert_eq!(failure.diagnostic().message(), Some("Fixture body failed"));
            assert!(failure
                .diagnostic()
                .causes()
                .iter()
                .any(|cause| cause.contains("Reader fixture cause")));
            assert_eq!(response.request_id.as_deref(), Some("fixture-request-id"));
            wait_for_abort().await;
            passed.push("reader-error-cause");
        }
        Ok(serde_json::to_string(&passed).expect("contract result JSON"))
    }
}
