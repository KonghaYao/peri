//! Bounded waits for both native Tokio and the JS event loop.
use std::{future::Future, time::Duration};

#[cfg(target_os = "emscripten")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, ms: u32) -> i32;
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = clearTimeout)]
    fn clear_timeout(id: i32);
}

#[cfg(target_os = "emscripten")]
struct Delay {
    fired: tokio::sync::oneshot::Receiver<()>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

#[cfg(target_os = "emscripten")]
impl Future for Delay {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        std::pin::Pin::new(&mut self.fired).poll(cx).map(|_| ())
    }
}

#[cfg(target_os = "emscripten")]
impl Drop for Delay {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

#[cfg(target_os = "emscripten")]
fn delay(budget: Duration) -> Delay {
    use wasm_bindgen::JsCast;
    let (fired_sender, fired) = tokio::sync::oneshot::channel();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let (done_sender, done) = tokio::sync::oneshot::channel();
    let milliseconds = budget.as_millis().min(u128::from(u32::MAX)) as u32;
    wasm_bindgen_futures::spawn_local(async move {
        let callback = wasm_bindgen::closure::Closure::once(move || {
            let _ = fired_sender.send(());
            let _ = done_sender.send(());
        });
        let timer = set_timeout(callback.as_ref().unchecked_ref(), milliseconds);
        tokio::select! {
            _ = cancelled => {},
            _ = done => {},
        }
        clear_timeout(timer);
        drop(callback);
    });
    Delay {
        fired,
        cancel: Some(cancel),
    }
}

pub(crate) async fn timeout<T>(budget: Duration, future: impl Future<Output = T>) -> Result<T, ()> {
    #[cfg(not(target_os = "emscripten"))]
    {
        tokio::time::timeout(budget, future).await.map_err(|_| ())
    }
    #[cfg(target_os = "emscripten")]
    {
        tokio::select! { result = future => Ok(result), _ = delay(budget) => Err(()) }
    }
}
