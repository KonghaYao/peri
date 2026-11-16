//! Identity shared by all events within one turn.

use serde::{Deserialize, Serialize};

// ─── TurnId ──────────────────────────────────────────────────────────────────

/// Turn 唯一标识符 — UUID v7（时间有序）
///
/// 作为一次 turn 内所有事件的统一纽带。从 LlmCallStart 到 TurnCompleted 全程一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(uuid::Uuid);

impl TurnId {
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    #[must_use]
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
