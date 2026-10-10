use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::Stream;
use js_sys::{Array, Promise, Reflect, Uint8Array};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use wasm_bindgen::{prelude::*, JsCast};
use wasm_bindgen_futures::{spawn_local, JsFuture};

use super::http::{HttpRequest, HttpResponse};
use crate::{ModelError, ModelResult, TransportErrorKind};

const QUEUED_CHUNKS: usize = 2;
const CHUNK_BYTES: usize = 64 * 1024;
const MAX_SOURCE_CHUNK_BYTES: u32 = 1024 * 1024;

#[wasm_bindgen(inline_js = r#"
export function periFetchBegin(url, method, headers, body) {
    const controller = new AbortController();
    const handle = { controller, reader: null };
    handle.response = globalThis.fetch(url, {
        method, headers, body: body === null ? undefined : body,
        signal: controller.signal, redirect: 'manual',
    });
    return handle;
}
export function periFetchResponse(handle) { return handle.response; }
export function periFetchMetadata(response) {
    return [response.status, response.headers.get('x-request-id') ?? response.headers.get('request-id')];
}
export function periFetchRead(handle, response) {
    if (!response.body) return Promise.resolve({done: true});
    handle.reader ??= response.body.getReader();
    return handle.reader.read();
}
export async function periFetchClose(handle, abort) {
    if (abort) handle.controller.abort();
    if (handle.reader) {
        try {
            if (abort) await handle.reader.cancel();
        } finally {
            handle.reader.releaseLock();
        }
    }
}
export function periFetchAbort(handle) { handle.controller.abort(); }
export function periFetchLogError(error) { console.error('Native fetch cleanup failed', error); }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = periFetchBegin, catch)]
    fn begin_fetch(
        url: &str,
        method: &str,
        headers: &JsValue,
        body: &JsValue,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = periFetchResponse)]
    fn fetch_response(handle: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = periFetchMetadata, catch)]
    fn fetch_metadata(response: &JsValue) -> Result<Array, JsValue>;
    #[wasm_bindgen(js_name = periFetchRead, catch)]
    fn fetch_read(handle: &JsValue, response: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = periFetchClose, catch)]
    fn fetch_close(handle: &JsValue, abort: bool) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = periFetchAbort, catch)]
    fn fetch_abort(handle: &JsValue) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = periFetchLogError)]
    fn log_error(error: &JsValue);
}

struct FetchRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

struct FetchOwnership(CancellationToken);

impl Drop for FetchOwnership {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct FetchBody {
    receiver: mpsc::Receiver<ModelResult<Vec<u8>>>,
    _ownership: FetchOwnership,
}

impl Stream for FetchBody {
    type Item = ModelResult<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

struct FetchHandle {
    handle: JsValue,
    closed: bool,
}

impl Drop for FetchHandle {
    fn drop(&mut self) {
        if !self.closed {
            if let Err(error) = fetch_abort(&self.handle) {
                log_error(&error);
            }
        }
    }
}

type ResponseMetadata = (u16, Option<String>);

pub(super) async fn send(
    request: HttpRequest,
    cancellation: CancellationToken,
) -> ModelResult<HttpResponse> {
    let headers = request
        .request
        .headers()
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_string(), value.to_string()))
                .map_err(|error| {
                    ModelError::transport(TransportErrorKind::Other, None::<&str>)
                        .with_error(&error)
                })
        })
        .collect::<ModelResult<Vec<_>>>()?;
    let body = request
        .request
        .body()
        .map(|body| {
            body.as_bytes().map(<[u8]>::to_vec).ok_or_else(|| {
                ModelError::transport(TransportErrorKind::Other, None::<&str>)
                    .with_message("Cloudflare model transport requires a buffered request body")
            })
        })
        .transpose()?;
    let request = FetchRequest {
        url: request.request.url().to_string(),
        method: request.request.method().as_str().to_string(),
        timeout: request.request.timeout().copied(),
        headers,
        body,
    };
    let ownership = FetchOwnership(CancellationToken::new());
    let stop = ownership.0.clone();
    let (response_sender, response_receiver) = oneshot::channel();
    let (body_sender, body_receiver) = mpsc::channel(QUEUED_CHUNKS);
    let local_cancellation = cancellation.clone();
    spawn_local(async move {
        let mut response_sender = Some(response_sender);
        let result = pump(
            request,
            local_cancellation,
            stop.clone(),
            &mut response_sender,
            &body_sender,
        )
        .await;
        if let Err(error) = result {
            if let Some(sender) = response_sender.take() {
                let _ = sender.send(Err(error));
            } else {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => {},
                    _ = body_sender.send(Err(error)) => {},
                }
            }
        }
    });
    let (status, request_id) = response_receiver.await.map_err(|error| {
        ModelError::transport(TransportErrorKind::Other, None::<&str>).with_error(&error)
    })??;
    Ok(HttpResponse::new(
        status,
        request_id,
        Box::pin(FetchBody {
            receiver: body_receiver,
            _ownership: ownership,
        }),
        cancellation,
    ))
}

async fn pump(
    request: FetchRequest,
    cancellation: CancellationToken,
    stop: CancellationToken,
    response_sender: &mut Option<oneshot::Sender<ModelResult<ResponseMetadata>>>,
    body_sender: &mpsc::Sender<ModelResult<Vec<u8>>>,
) -> ModelResult<()> {
    if cancellation.is_cancelled() || stop.is_cancelled() {
        return Err(ModelError::cancelled());
    }
    let deadline = async {
        match request.timeout {
            Some(timeout) => peri_time::sleep(timeout).await,
            None => std::future::pending::<()>().await,
        }
    };
    let stopped = async {
        tokio::select! {
            biased;
            _ = stop.cancelled() => ModelError::cancelled(),
            _ = cancellation.cancelled() => ModelError::cancelled(),
            _ = deadline => ModelError::transport(TransportErrorKind::Timeout, None::<&str>)
                .with_message("Cloudflare model request deadline exceeded"),
        }
    };
    tokio::pin!(stopped);
    let headers = Array::new();
    for (name, value) in request.headers {
        let pair = Array::new();
        pair.push(&JsValue::from_str(&name));
        pair.push(&JsValue::from_str(&value));
        headers.push(&pair);
    }
    let body = request
        .body
        .as_ref()
        .map(|body| Uint8Array::from(body.as_slice()).into())
        .unwrap_or(JsValue::NULL);
    let mut handle = FetchHandle {
        handle: begin_fetch(&request.url, &request.method, &headers, &body)
            .map_err(|error| map_js_error(error, TransportErrorKind::Connection))?,
        closed: false,
    };
    let result = async {
    let response = tokio::select! {
        biased;
        error = &mut stopped => return Err(error),
        response = JsFuture::from(fetch_response(&handle.handle)) => response
            .map_err(|error| map_js_error(error, TransportErrorKind::Connection))?,
    };
    let metadata = fetch_metadata(&response)
        .map_err(|error| map_js_error(error, TransportErrorKind::Other))?;
    let status = metadata
        .get(0)
        .as_f64()
        .filter(|status| status.fract() == 0.0 && (100.0..=599.0).contains(status))
        .ok_or_else(|| {
            ModelError::transport(TransportErrorKind::Other, None::<&str>)
                .with_message("Invalid native fetch response status")
        })? as u16;
    response_sender
        .take()
        .expect("response sender owned until headers")
        .send(Ok((status, metadata.get(1).as_string())))
        .map_err(|_| ModelError::cancelled())?;
    loop {
        let reading = fetch_read(&handle.handle, &response)
            .map_err(|error| map_js_error(error, TransportErrorKind::Other))?;
        let result = tokio::select! {
            biased;
            error = &mut stopped => return Err(error),
            result = JsFuture::from(reading) => result.map_err(|error| map_js_error(error, TransportErrorKind::Other))?,
        };
        if Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|error| map_js_error(error, TransportErrorKind::Other))?
            .as_bool()
            == Some(true)
        {
            return Ok(());
        }
        let chunk = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|error| map_js_error(error, TransportErrorKind::Other))?
            .dyn_into::<Uint8Array>()
            .map_err(|error| map_js_error(error, TransportErrorKind::Other))?;
        if chunk.length() > MAX_SOURCE_CHUNK_BYTES {
            return Err(
                ModelError::transport(TransportErrorKind::Other, None::<&str>)
                    .with_message("Native fetch source chunk exceeds the 1 MiB transport limit"),
            );
        }
        for offset in (0..chunk.length()).step_by(CHUNK_BYTES) {
            let bytes = chunk
                .subarray(offset, (offset + CHUNK_BYTES as u32).min(chunk.length()))
                .to_vec();
            tokio::select! {
                biased;
                error = &mut stopped => return Err(error),
                sent = body_sender.send(Ok(bytes)) => sent.map_err(|_| ModelError::cancelled())?,
            }
        }
    }
    }.await;
    let cleanup = async {
        let closing = fetch_close(&handle.handle, result.is_err())?;
        JsFuture::from(closing).await.map(|_| ())
    };
    match peri_time::timeout(Duration::from_secs(1), cleanup).await {
        Ok(Ok(())) => handle.closed = true,
        Ok(Err(error)) => {
            log_error(&error);
            if result.is_ok() {
                return Err(map_js_error(error, TransportErrorKind::Other));
            }
        }
        Err(error) => {
            let failure = ModelError::transport(TransportErrorKind::Timeout, None::<&str>)
                .with_message("Native fetch reader cleanup deadline exceeded")
                .with_error(&error);
            log_error(&JsValue::from_str(&failure.to_string()));
            if result.is_ok() {
                return Err(failure);
            }
        }
    }
    result
}

fn map_js_error(error: JsValue, fallback: TransportErrorKind) -> ModelError {
    let field = |value: &JsValue, name: &str| Reflect::get(value, &JsValue::from_str(name)).ok();
    let name = field(&error, "name").and_then(|value| value.as_string());
    let mapped = match name.as_deref() {
        Some("AbortError") => ModelError::cancelled(),
        Some("TimeoutError") => ModelError::transport(TransportErrorKind::Timeout, None::<&str>),
        _ => ModelError::transport(fallback, None::<&str>),
    };
    let message = field(&error, "message")
        .and_then(|value| value.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("Native fetch JavaScript failure: {error:?}"));
    let mut causes = Vec::new();
    if let Some(stack) = field(&error, "stack").and_then(|value| value.as_string()) {
        causes.push(stack);
    }
    let mut cause = field(&error, "cause");
    for _depth in 0..16 {
        let Some(value) = cause.filter(|value| !value.is_null() && !value.is_undefined()) else {
            break;
        };
        causes.push(
            field(&value, "message")
                .and_then(|value| value.as_string())
                .or_else(|| value.as_string())
                .unwrap_or_else(|| format!("{value:?}")),
        );
        cause = field(&value, "cause");
    }
    mapped.with_message(message).with_causes(causes)
}
