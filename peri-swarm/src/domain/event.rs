use serde::{Deserialize, Serialize};

use crate::domain::item::ItemView;

/// Turn lifecycle event carried by SSE `event: turn` (`design §2.5`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnEvent {
    Started {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    Completed {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
    Failed {
        #[serde(rename = "requestId")]
        request_id: String,
        error: TurnFailure,
    },
}

impl TurnEvent {
    #[must_use]
    pub fn started(request_id: impl Into<String>) -> Self {
        Self::Started {
            request_id: request_id.into(),
        }
    }

    #[must_use]
    pub fn completed(request_id: impl Into<String>, stop_reason: impl Into<String>) -> Self {
        Self::Completed {
            request_id: request_id.into(),
            stop_reason: stop_reason.into(),
        }
    }

    #[must_use]
    pub fn failed(request_id: impl Into<String>, code: i64, message: impl Into<String>) -> Self {
        Self::Failed {
            request_id: request_id.into(),
            error: TurnFailure {
                code,
                message: message.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnFailure {
    pub code: i64,
    pub message: String,
}

/// Everything the SSE layer can emit for a session.
///
/// Only [`StreamEvent::Item`] carries a persisted `seq`; it is the only event
/// that advances `Last-Event-ID`. `turn`/`gap` events have no `id`, because
/// `swarm_items` is the single replayable record (`design §6.1`).
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Item(ItemView),
    Turn(TurnEvent),
    Gap { from_seq: u64, to_seq: u64 },
}

impl StreamEvent {
    #[must_use]
    pub fn item_seq(&self) -> Option<u64> {
        match self {
            Self::Item(item) => Some(item.seq),
            Self::Turn(_) | Self::Gap { .. } => None,
        }
    }
}
