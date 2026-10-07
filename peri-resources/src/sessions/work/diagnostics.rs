use std::time::Instant;

pub(in crate::sessions) struct WorkPhase {
    started: Instant,
    backend: &'static str,
    phase: &'static str,
    pub(in crate::sessions) state_bytes: usize,
    pub(in crate::sessions) query_count: usize,
    pub(in crate::sessions) transaction_count: usize,
}

impl WorkPhase {
    pub(in crate::sessions) fn new(backend: &'static str, phase: &'static str) -> Self {
        Self {
            started: Instant::now(),
            backend,
            phase,
            state_bytes: 0,
            query_count: 0,
            transaction_count: 0,
        }
    }
}

impl Drop for WorkPhase {
    fn drop(&mut self) {
        tracing::debug!(
            target: "peri_resources::work_phase",
            backend = self.backend,
            phase = self.phase,
            elapsed_us = self.started.elapsed().as_micros() as u64,
            state_bytes = self.state_bytes,
            submitted_sql_count = self.query_count,
            transaction_scope_count = self.transaction_count,
            "work persistence phase exited"
        );
    }
}
