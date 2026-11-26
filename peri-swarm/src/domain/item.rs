use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::domain::error::{ErrorCode, ErrorKind, SwarmError};
use crate::domain::official_item::OfficialSessionItem;

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
    /// Durable approval fact: an `mcp_approval_request` was opened or settled.
    ///
    /// It is a custom item, so it never becomes an official turn output; its
    /// `seq` is what makes the approval discoverable on reconnect.
    Approval,
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
            Self::Approval => "approval",
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

    /// Whether this item belongs to the root agent rather than a child.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.agent_id == "root"
    }
}

/// One append-only item. `seq` is the stable ordering and resume cursor.
///
/// `id` mirrors `itemId` so SDK types that extend `{ id }` can traverse the
/// item log; `seq` stays the pagination cursor (`after`/`nextCursor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    pub id: String,
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
///
/// `after_id` is the official SDK spelling: `CursorPage` sends the last item's
/// `id` (`itim_...`) in `after`. Services resolve it to that item's greatest
/// `seq` before querying; a numeric `after`/`afterSeq` keeps its old meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemsWindow {
    pub order: ItemOrder,
    pub after: Option<u64>,
    pub after_id: Option<String>,
    pub limit: u32,
}

impl ItemsWindow {
    pub const DEFAULT_LIMIT: u32 = 200;
    pub const MAX_LIMIT: u32 = 500;
}

/// One page of persisted items with a stable cursor.
///
/// 分页以条目为单位（`design §4.2`）。
/// 选中条目的所有原始行都在 `items`；`data` 是同一页的官方投影，
/// 每个可映射条目最多一个元素。`has_more`/`hasMore` 仍按条目计数，
/// `nextCursor` 仍指向页尾条目的最大 `seq`。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPage {
    pub logical_id: String,
    pub object: String,
    /// Raw rows of the page's items, in `order`.
    pub items: Vec<ItemView>,
    /// 可映射的官方条目，保持页内条目顺序。
    pub data: Vec<OfficialSessionItem>,
    /// Last item's greatest `seq` when more items exist; `null` otherwise.
    pub next_cursor: Option<u64>,
    pub last_item_seq: u64,
    pub has_more: bool,
    pub order: ItemOrder,
}

/// Serialized by hand so one `has_more` value is emitted under both the
/// camelCase name and the official snake_case name.
impl Serialize for ItemPage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut page = serializer.serialize_struct("ItemPage", 9)?;
        page.serialize_field("logicalId", &self.logical_id)?;
        page.serialize_field("object", &self.object)?;
        page.serialize_field("items", &self.items)?;
        page.serialize_field("data", &self.data)?;
        page.serialize_field("nextCursor", &self.next_cursor)?;
        page.serialize_field("lastItemSeq", &self.last_item_seq)?;
        page.serialize_field("hasMore", &self.has_more)?;
        page.serialize_field("has_more", &self.has_more)?;
        page.serialize_field("order", &self.order)?;
        page.end()
    }
}

impl ItemPage {
    /// `items` 是原始行，`data` 是同一页的官方条目投影。
    #[must_use]
    pub fn new(
        logical_id: impl Into<String>,
        items: Vec<ItemView>,
        data: Vec<OfficialSessionItem>,
        next_cursor: Option<u64>,
        last_item_seq: u64,
        has_more: bool,
        order: ItemOrder,
    ) -> Self {
        Self {
            logical_id: logical_id.into(),
            object: "list".to_owned(),
            items,
            data,
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
