use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::domain::error::{ErrorCode, ErrorKind, SwarmError};

/// Maximum serialized payload retained in `swarm_items.payload_json`.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Item kinds exposed by the Swarm contract (`design §2.5`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Message,
    Thought,
    ToolCall,
    ToolUpdate,
    ToolResult,
    Receipt,
}

impl ItemKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Thought => "thought",
            Self::ToolCall => "tool_call",
            Self::ToolUpdate => "tool_update",
            Self::ToolResult => "tool_result",
            Self::Receipt => "receipt",
        }
    }
}

/// Origin of an item. `agentId` is `root` or `child:<sessionId>`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSource {
    pub agent_id: String,
    pub task_id: Option<String>,
}

impl ItemSource {
    #[must_use]
    pub fn root() -> Self {
        Self {
            agent_id: "root".to_owned(),
            task_id: None,
        }
    }
}

/// One append-only item. `seq` is the stable ordering and resume cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    pub seq: u64,
    pub item_id: String,
    pub kind: ItemKind,
    pub source: ItemSource,
    pub payload: Value,
    pub created_at: String,
    pub truncated: bool,
}

impl ItemView {
    /// Payload is truncated to a marker when it exceeds [`MAX_PAYLOAD_BYTES`];
    /// `itemId`/`seq`/`kind` stay intact (`design §2.6`).
    #[must_use]
    pub fn fit_payload(payload: Value) -> (Value, bool) {
        let encoded = serde_json::to_vec(&payload).unwrap_or_default();
        if encoded.len() <= MAX_PAYLOAD_BYTES {
            return (payload, false);
        }
        (
            serde_json::json!({
                "_truncated": true,
                "originalBytes": encoded.len(),
            }),
            true,
        )
    }

    #[must_use]
    pub fn new_item_id() -> String {
        format!("itim_{}", Uuid::now_v7())
    }
}

/// Query window for `GET /items` (`design §2.6` plus the SDK `order` field).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrder {
    Asc,
    Desc,
}

impl ItemOrder {
    pub fn parse(value: &str) -> Result<Self, SwarmError> {
        match value {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            other => Err(SwarmError::new(
                ErrorKind::BadRequest,
                ErrorCode::InvalidQuery,
                "order must be `asc` or `desc`",
            )
            .with_details(serde_json::json!({ "received": other }))),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// `order=asc` reads `seq > after`; `order=desc` reads `seq < after` (or from
/// the newest item when `after` is absent).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemsWindow {
    pub order: ItemOrder,
    pub after: Option<u64>,
    pub limit: u32,
}

impl ItemsWindow {
    pub const DEFAULT_LIMIT: u32 = 200;
    pub const MAX_LIMIT: u32 = 500;
}

/// One page of persisted items with a stable cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPage {
    pub logical_id: String,
    pub object: String,
    pub items: Vec<ItemView>,
    /// Last seq of this page when more rows exist; `null` otherwise.
    pub next_cursor: Option<u64>,
    pub last_item_seq: u64,
    pub has_more: bool,
    pub order: ItemOrder,
}

impl ItemPage {
    #[must_use]
    pub fn new(
        logical_id: impl Into<String>,
        items: Vec<ItemView>,
        next_cursor: Option<u64>,
        last_item_seq: u64,
        has_more: bool,
        order: ItemOrder,
    ) -> Self {
        Self {
            logical_id: logical_id.into(),
            object: "list".to_owned(),
            items,
            next_cursor,
            last_item_seq,
            has_more,
            order,
        }
    }
}

#[cfg(test)]
#[path = "item_test.rs"]
mod tests;
