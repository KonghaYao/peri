use std::{future::pending, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn run_smoke() -> Result<String, JsValue> {
    let ready = peri_time::timeout(Duration::ZERO, async { 7 }).await;
    if ready != Ok(7) {
        return Err(JsValue::from_str("ready future lost zero-budget race"));
    }
    peri_time::sleep(Duration::from_millis(2)).await;
    let expired = peri_time::timeout(Duration::from_millis(2), pending::<()>()).await;
    if expired != Err(peri_time::Elapsed) {
        return Err(JsValue::from_str("pending future did not expire"));
    }
    let mut ticker = peri_time::interval(Duration::from_millis(2));
    ticker.set_missed_tick_behavior(peri_time::MissedTickBehavior::Skip);
    let _ = ticker.tick().await;
    let _ = ticker.tick().await;
    Ok("ok".to_string())
}

/// Poll then cancel two pending waits so the JS harness can inspect cleanup.
#[wasm_bindgen]
pub fn test_timer_edges() -> Result<(), JsValue> {
    use std::{future::Future, task::{Context, Poll, Waker}};
    for duration in [Duration::from_nanos(1), Duration::from_millis(i32::MAX as u64 + 10)] {
        let mut sleep = Box::pin(peri_time::sleep(duration));
        let mut context = Context::from_waker(Waker::noop());
        if !matches!(sleep.as_mut().poll(&mut context), Poll::Pending) {
            return Err(JsValue::from_str("sleep unexpectedly completed"));
        }
        drop(sleep);
    }
    Ok(())
}

fn main() {}
