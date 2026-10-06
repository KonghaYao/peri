use super::{WorkLimits, WorkState};

pub const DEFAULT_AGENT_MAX_ITERATIONS: usize = 500;
pub(super) const DEFAULT_DISPATCH_BUDGET: u64 = DEFAULT_AGENT_MAX_ITERATIONS as u64 * 4;

impl WorkState {
    pub fn upgrade_default_budget_policy_for_user_selection(&mut self) -> bool {
        let legacy_default = WorkLimits {
            required_deliveries: 1024,
            optional_deliveries: 128,
            required_bytes: 8 * 1024 * 1024,
            optional_bytes: 512 * 1024,
            max_batch_size: 64,
            reason_requests: 64,
            dispatches: 256,
            recoveries: 8,
        };
        if self.limits != legacy_default {
            return false;
        }
        self.limits.reason_requests = DEFAULT_AGENT_MAX_ITERATIONS as u64;
        self.limits.dispatches = DEFAULT_DISPATCH_BUDGET;
        true
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
