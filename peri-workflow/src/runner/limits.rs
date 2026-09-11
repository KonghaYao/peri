//! 活跃 agent attempt 配额；permit 由执行 task 持有并在 Drop 时归还。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub(super) struct LiveAttemptPermit {
    counter: Arc<AtomicU64>,
}

impl Drop for LiveAttemptPermit {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn try_reserve_live_attempt(
    counter: &Arc<AtomicU64>,
    maximum: Option<u64>,
) -> Option<LiveAttemptPermit> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            let next = current.checked_add(1)?;
            (!maximum.is_some_and(|maximum| next > maximum)).then_some(next)
        })
        .ok()
        .map(|_| LiveAttemptPermit {
            counter: Arc::clone(counter),
        })
}
