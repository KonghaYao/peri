use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionState {
    Open,
    Draining,
    Closed,
}

struct AdmissionInner {
    state: Mutex<(AdmissionState, usize)>,
    notify: Notify,
}

#[derive(Clone)]
pub struct DynamicMcpAdmissionGate {
    inner: Arc<AdmissionInner>,
}

impl Default for DynamicMcpAdmissionGate {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicMcpAdmissionGate {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AdmissionInner {
                state: Mutex::new((AdmissionState::Open, 0)),
                notify: Notify::new(),
            }),
        }
    }

    pub fn state(&self) -> AdmissionState {
        self.inner.state.lock().0
    }

    pub fn try_acquire(&self) -> Result<DynamicMcpPermit, AdmissionState> {
        let mut state = self.inner.state.lock();
        if state.0 != AdmissionState::Open {
            return Err(state.0);
        }
        state.1 += 1;
        Ok(DynamicMcpPermit {
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn begin_draining(&self) -> bool {
        let mut state = self.inner.state.lock();
        if state.0 == AdmissionState::Open {
            state.0 = AdmissionState::Draining;
            if state.1 == 0 {
                self.inner.notify.notify_waiters();
            }
            true
        } else {
            false
        }
    }

    pub async fn drain(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.inner.state.lock().1 == 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn close(&self) {
        let mut state = self.inner.state.lock();
        state.0 = AdmissionState::Closed;
        if state.1 == 0 {
            self.inner.notify.notify_waiters();
        }
    }
}

pub struct DynamicMcpPermit {
    inner: Arc<AdmissionInner>,
}

impl Drop for DynamicMcpPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        state.1 = state.1.saturating_sub(1);
        if state.1 == 0 {
            self.inner.notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn draining_rejects_new_permits_and_waits_for_existing() {
        let gate = DynamicMcpAdmissionGate::new();
        let permit = gate.try_acquire().unwrap();
        assert!(gate.begin_draining());
        assert!(matches!(gate.try_acquire(), Err(AdmissionState::Draining)));
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.drain().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(permit);
        waiter.await.unwrap();
    }
}
