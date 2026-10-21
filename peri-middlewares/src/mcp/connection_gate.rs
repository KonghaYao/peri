//! Shared admission for every consumer of a concrete MCP connection.
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::Notify;
#[derive(Clone)]
pub struct McpCallGate {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<(bool, usize)>,
    changed: Notify,
}
impl Default for McpCallGate {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new((true, 0)),
                changed: Notify::new(),
            }),
        }
    }
}
impl McpCallGate {
    pub fn is_open(&self) -> bool {
        self.inner.state.lock().0
    }
    pub fn try_acquire(&self) -> Result<McpCallPermit, &'static str> {
        let mut state = self.inner.state.lock();
        if !state.0 {
            return Err("MCP connection is stopping");
        }
        state.1 += 1;
        Ok(McpCallPermit {
            inner: self.inner.clone(),
        })
    }
    pub(crate) fn revoke(&self) {
        self.inner.state.lock().0 = false;
        self.inner.changed.notify_waiters();
    }
    pub(crate) async fn drain(&self) {
        loop {
            let changed = self.inner.changed.notified();
            if self.inner.state.lock().1 == 0 {
                return;
            }
            changed.await;
        }
    }
}
pub struct McpCallPermit {
    inner: Arc<Inner>,
}
impl Drop for McpCallPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        state.1 -= 1;
        if state.1 == 0 {
            self.inner.changed.notify_waiters();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn revoked_old_handle_rejects_and_drains() {
        let gate = McpCallGate::default();
        let call = gate.try_acquire().unwrap();
        gate.revoke();
        assert!(gate.try_acquire().is_err());
        assert!(
            peri_time::timeout(std::time::Duration::from_millis(5), gate.drain())
                .await
                .is_err()
        );
        drop(call);
        gate.drain().await;
    }
}
