use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(crate) struct SessionActivation {
    pub(crate) listener_started: AtomicBool,
    allowed: AtomicBool,
}

impl SessionActivation {
    pub(crate) fn allow(&self) {
        self.allowed.store(true, Ordering::Release);
    }

    pub(crate) fn suppress(&self) {
        self.allowed.store(false, Ordering::Release);
    }

    pub(crate) fn is_allowed(&self) -> bool {
        self.allowed.load(Ordering::Acquire)
    }
}
