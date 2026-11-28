//! Connection-pool capacity for one remote session-store facade.
//!
//! The default keeps the historical single-facade setting. A deployment that
//! shares one facade across many concurrent sessions (the Swarm gateway) raises
//! the cap explicitly instead of changing every TUI/CLI/stdio open.

/// Upper bound for one remote session store's SQLx pool.
///
/// `max_connections` is a cap, not a reservation: the pool still opens with
/// `min_connections(0)` and returns to zero when idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStorePoolPolicy {
    max_connections: u32,
}

impl SessionStorePoolPolicy {
    /// # Panics
    ///
    /// Panics when `max_connections` is zero: a pool that cannot admit one
    /// connection would turn every session access into a timeout.
    #[must_use]
    pub const fn new(max_connections: u32) -> Self {
        assert!(
            max_connections > 0,
            "session store pool policy requires max_connections > 0"
        );
        Self { max_connections }
    }

    #[must_use]
    pub const fn max_connections(self) -> u32 {
        self.max_connections
    }
}

impl Default for SessionStorePoolPolicy {
    fn default() -> Self {
        Self::new(2)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionStorePoolPolicy;

    #[test]
    fn default_keeps_the_single_facade_capacity() {
        assert_eq!(SessionStorePoolPolicy::default().max_connections(), 2);
    }

    #[test]
    fn explicit_capacity_is_preserved() {
        assert_eq!(SessionStorePoolPolicy::new(32).max_connections(), 32);
    }

    #[test]
    #[should_panic(expected = "max_connections > 0")]
    fn zero_is_rejected() {
        let _ = SessionStorePoolPolicy::new(0);
    }
}
