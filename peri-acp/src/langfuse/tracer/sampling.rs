//! 采样决策器。
//!
//! 基于稳定哈希的 turn 级采样，确保同一 turn 内的多个 on_* 调用
//! 保持一致的 emit/drop 决策。
//!
//! - should_emit：首次调用时基于 (turn_id, session_id) 哈希计算决策并缓存
//! - cleanup_turn：turn 结束后清理缓存，防止内存泄漏
//! - 紧急清理：缓存条目超过 1000 时自动裁剪

use std::collections::HashMap;

const EMERGENCY_CLEANUP_THRESHOLD: usize = 1000;
const EMERGENCY_CLEANUP_KEEP: usize = 500;

pub(crate) struct SamplingDecider {
    rate: f64,
    decided: HashMap<String, bool>,
}

impl SamplingDecider {
    pub(crate) fn new(rate: f64) -> Self {
        Self {
            rate: rate.clamp(0.0, 1.0),
            decided: HashMap::new(),
        }
    }

    pub(crate) fn should_emit(&mut self, turn_id: &str, session_id: &str) -> bool {
        if let Some(d) = self.decided.get(turn_id) {
            return *d;
        }

        if self.decided.len() > EMERGENCY_CLEANUP_THRESHOLD {
            self.emergency_cleanup();
        }

        let h = stable_hash(turn_id, session_id);
        let decision = (h % 10_000) as f64 / 10_000.0 < self.rate;
        self.decided.insert(turn_id.to_string(), decision);
        decision
    }

    pub(crate) fn cleanup_turn(&mut self, turn_id: &str) {
        self.decided.remove(turn_id);
    }

    pub(crate) fn decided_len(&self) -> usize {
        self.decided.len()
    }

    fn emergency_cleanup(&mut self) {
        if self.decided.len() <= EMERGENCY_CLEANUP_KEEP {
            return;
        }
        let keep: Vec<String> = self
            .decided
            .keys()
            .skip(self.decided.len() - EMERGENCY_CLEANUP_KEEP)
            .cloned()
            .collect();
        let kept: HashMap<String, bool> = keep
            .into_iter()
            .filter_map(|k| self.decided.get(&k).map(|v| (k, *v)))
            .collect();
        self.decided = kept;
    }
}

fn stable_hash(turn_id: &str, session_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    turn_id.hash(&mut h);
    session_id.hash(&mut h);
    h.finish()
}

#[cfg(test)]
#[path = "sampling_test.rs"]
mod tests;
