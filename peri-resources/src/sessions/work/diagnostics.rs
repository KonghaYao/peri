use std::time::Instant;

pub(in crate::sessions) struct WorkPhase {
    started: Instant,
    backend: &'static str,
    phase: &'static str,
    pub(in crate::sessions) state_bytes: usize,
    pub(in crate::sessions) query_count: usize,
    pub(in crate::sessions) transaction_count: usize,
    pub(in crate::sessions) command_bytes: usize,
    pub(in crate::sessions) encoded_state_bytes: usize,
    pub(in crate::sessions) sql_parameter_bytes: usize,
    pub(in crate::sessions) unique_parameter_buffer_bytes: usize,
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
            command_bytes: 0,
            encoded_state_bytes: 0,
            sql_parameter_bytes: 0,
            unique_parameter_buffer_bytes: 0,
        }
    }

    pub(in crate::sessions) fn record_effects(&mut self, effects: &[super::WorkEffect]) {
        if tracing::enabled!(target: "peri_resources::work_phase", tracing::Level::DEBUG) {
            let mut buffers = std::collections::HashSet::new();
            for effect in effects {
                for parameter in &effect.params {
                    self.sql_parameter_bytes += parameter.len();
                    if buffers.insert((parameter.as_ptr(), parameter.len())) {
                        self.unique_parameter_buffer_bytes += parameter.len();
                    }
                }
                if effect.sql == super::UPDATE_STATE {
                    self.encoded_state_bytes += effect.params[1].len();
                }
            }
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
            command_bytes = self.command_bytes,
            encoded_state_bytes = self.encoded_state_bytes,
            sql_parameter_bytes = self.sql_parameter_bytes,
            unique_parameter_buffer_bytes = self.unique_parameter_buffer_bytes,
            submitted_sql_count = self.query_count,
            transaction_scope_count = self.transaction_count,
            "work persistence phase exited"
        );
    }
}
