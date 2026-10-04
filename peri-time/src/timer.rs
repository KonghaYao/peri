use std::{future::Future, time::Duration};

pub use std::time::Instant;

/// The supplied monotonic deadline expired before the future completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("deadline elapsed")]
pub struct Elapsed;

/// Wait for a duration. Dropping the returned future cancels its timer.
pub async fn sleep(duration: Duration) {
    #[cfg(not(target_os = "emscripten"))]
    tokio::time::sleep(duration).await;
    #[cfg(target_os = "emscripten")]
    {
        let mut remaining = duration;
        while !remaining.is_zero() {
            let chunk = remaining.min(Duration::from_millis(i32::MAX as u64));
            js::Delay::new(chunk).await;
            remaining -= chunk;
        }
    }
}

/// Wait until a process-local monotonic deadline.
pub async fn sleep_until(deadline: Instant) {
    #[cfg(not(target_os = "emscripten"))]
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    #[cfg(target_os = "emscripten")]
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if remaining.is_zero() {
            break;
        }
        js::Delay::new(remaining.min(Duration::from_millis(i32::MAX as u64))).await;
    }
}

/// Run a future until a relative budget expires. The future is dropped on expiry.
pub async fn timeout<F: Future>(budget: Duration, future: F) -> crate::Result<F::Output> {
    #[cfg(not(target_os = "emscripten"))]
    {
        tokio::time::timeout(budget, future)
            .await
            .map_err(|_| Elapsed)
    }
    #[cfg(target_os = "emscripten")]
    {
        if let Some(deadline) = Instant::now().checked_add(budget) {
            timeout_at(deadline, future).await
        } else {
            // An unrepresentable deadline is beyond any useful process lifetime.
            Ok(future.await)
        }
    }
}

/// Run a future until an absolute process-local monotonic deadline.
pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> crate::Result<F::Output> {
    #[cfg(not(target_os = "emscripten"))]
    {
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future)
            .await
            .map_err(|_| Elapsed)
    }
    #[cfg(target_os = "emscripten")]
    {
        tokio::select! {
            biased;
            result = future => Ok(result),
            _ = sleep_until(deadline) => Err(Elapsed),
        }
    }
}

/// A periodic process-local timer. Its first tick completes immediately.
pub struct Interval {
    #[cfg(not(target_os = "emscripten"))]
    native: tokio::time::Interval,
    #[cfg(target_os = "emscripten")]
    next: Instant,
    #[cfg(target_os = "emscripten")]
    period: Duration,
    #[cfg(target_os = "emscripten")]
    missed_tick_behavior: MissedTickBehavior,
}

/// How an interval schedules its next tick after falling behind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissedTickBehavior {
    /// Emit overdue ticks promptly until the original schedule catches up.
    Burst,
    /// Start a fresh period after a missed tick.
    Delay,
    /// Preserve the original phase and skip missed ticks.
    Skip,
}

/// Create a periodic timer. A zero period panics, matching Tokio's contract.
pub fn interval(period: Duration) -> Interval {
    assert!(!period.is_zero(), "interval period must be nonzero");
    #[cfg(not(target_os = "emscripten"))]
    {
        Interval {
            native: tokio::time::interval(period),
        }
    }
    #[cfg(target_os = "emscripten")]
    {
        Interval {
            next: Instant::now(),
            period,
            missed_tick_behavior: MissedTickBehavior::Burst,
        }
    }
}

impl Interval {
    /// Choose how missed ticks affect the next scheduled deadline.
    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        #[cfg(not(target_os = "emscripten"))]
        self.native.set_missed_tick_behavior(match behavior {
            MissedTickBehavior::Burst => tokio::time::MissedTickBehavior::Burst,
            MissedTickBehavior::Delay => tokio::time::MissedTickBehavior::Delay,
            MissedTickBehavior::Skip => tokio::time::MissedTickBehavior::Skip,
        });
        #[cfg(target_os = "emscripten")]
        {
            self.missed_tick_behavior = behavior;
        }
    }

    /// Wait for the next tick and return its scheduled monotonic instant.
    pub async fn tick(&mut self) -> Instant {
        #[cfg(not(target_os = "emscripten"))]
        {
            self.native.tick().await.into_std()
        }
        #[cfg(target_os = "emscripten")]
        {
            let scheduled = self.next;
            sleep_until(scheduled).await;
            let candidate = scheduled
                .checked_add(self.period)
                .expect("interval deadline overflow");
            let now = Instant::now();
            self.next = if candidate > now {
                candidate
            } else {
                match self.missed_tick_behavior {
                    MissedTickBehavior::Burst => candidate,
                    MissedTickBehavior::Delay => now
                        .checked_add(self.period)
                        .expect("interval deadline overflow"),
                    MissedTickBehavior::Skip => {
                        let missed =
                            now.duration_since(candidate).as_nanos() / self.period.as_nanos() + 1;
                        if let Ok(missed) = u32::try_from(missed) {
                            candidate
                                .checked_add(
                                    self.period
                                        .checked_mul(missed)
                                        .expect("interval period overflow"),
                                )
                                .expect("interval deadline overflow")
                        } else {
                            now.checked_add(self.period)
                                .expect("interval deadline overflow")
                        }
                    }
                }
            };
            scheduled
        }
    }
}

#[cfg(target_os = "emscripten")]
mod js {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
        time::Duration,
    };
    use wasm_bindgen::{closure::Closure, JsCast};

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
}

#[cfg(test)]
#[path = "timer_test.rs"]
mod tests;
