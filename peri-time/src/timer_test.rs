use super::*;
use std::future::{pending, ready};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct PendingWithDrop(Arc<AtomicBool>);

impl Future for PendingWithDrop {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Pending
    }
}

impl Drop for PendingWithDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn test_ready_future_wins_zero_budget() {
    assert_eq!(timeout(Duration::ZERO, ready(7)).await, Ok(7));
}

#[tokio::test]
async fn test_pending_future_expires_at_past_deadline() {
    let deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(timeout_at(deadline, pending::<()>()).await, Err(Elapsed));
}

#[tokio::test]
async fn test_expiry_drops_business_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let result = timeout(Duration::ZERO, PendingWithDrop(dropped.clone())).await;
    assert_eq!(result, Err(Elapsed));
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_interval_first_tick_is_immediate() {
    let mut ticker = interval(Duration::from_secs(1));
    assert!(timeout(Duration::ZERO, ticker.tick()).await.is_ok());
}

#[cfg(not(target_os = "emscripten"))]
#[tokio::test(start_paused = true)]
async fn test_monotonic_now_tracks_tokio_paused_clock() {
    let started = crate::monotonic_now();
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(crate::elapsed_since(started), Duration::from_secs(3));
}
