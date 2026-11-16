use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, ms: u32) -> i32;
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = clearTimeout)]
    fn clear_timeout(id: i32);
}

pub(super) struct Delay {
    receiver: tokio::sync::oneshot::Receiver<()>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Delay {
    pub(super) fn new(duration: Duration) -> Self {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        // A positive sub-millisecond budget must not expire early.
        let ms = duration.as_millis() + u128::from(duration.subsec_nanos() % 1_000_000 != 0);
        wasm_bindgen_futures::spawn_local(async move {
            let (done_sender, done) = tokio::sync::oneshot::channel();
            let callback = Closure::once(move || {
                let _ = sender.send(());
                let _ = done_sender.send(());
            });
            let timer = set_timeout(
                callback.as_ref().unchecked_ref(),
                ms.min(i32::MAX as u128) as u32,
            );
            tokio::select! {
                _ = cancelled => {},
                _ = done => {},
            }
            clear_timeout(timer);
            drop(callback);
        });
        Self {
            receiver,
            cancel: Some(cancel),
        }
    }
}

impl Future for Delay {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.receiver).poll(cx).map(|_| ())
    }
}

impl Drop for Delay {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
