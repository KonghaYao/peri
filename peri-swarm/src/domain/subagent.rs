//! Official `sessions.subagents` shapes (`openai@7.60.0` Agents API).
//!
//! A Peri child thread is the only persisted fact behind a subagent: the
//! gateway projects `ThreadMeta` into the official `Subagent` object without
//! adding a second identity or a subagent table.

use serde::{Deserialize, Serialize};

use crate::domain::item::ItemOrder;

/// Official `Subagent.object`.
pub const SUBAGENT_OBJECT: &str = "agent.session.subagent";

/// Query window for `GET /v1/agents/sessions/{id}/subagents`
/// (`SubagentListParams`).
///
/// `after` is a subagent id and is exclusive in the selected order. Subagents
/// are ordered by `(opened_at, id)`: `opened_at` is second-granular, so the id
/// is the tie-break for two children opened in the same second.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentsWindow {
    pub order: ItemOrder,
    pub after: Option<String>,
    pub limit: u32,
}

impl SubagentsWindow {
    pub const DEFAULT_LIMIT: u32 = 20;
    pub const MAX_LIMIT: u32 = 500;
}

/// One official `Subagent`.
///
/// `name` and `closed_at` are nullable in the SDK; `instructions` is always
/// `null` because the gateway does not persist a subagent's opening task as
/// official content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialSubagent {
    pub id: String,
    pub object: String,
    pub session_id: String,
    pub parent_agent_id: String,
    pub name: Option<String>,
    pub instructions: Option<Vec<serde_json::Value>>,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
    pub status: String,
}

/// Neutral read model between the session store and the official projection.
///
/// The reader fills it from `ThreadMeta`; the service maps it to
/// [`OfficialSubagent`] with the owning session's public identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentRecord {
    pub id: String,
    pub parent_thread_id: Option<String>,
    pub name: Option<String>,
    /// `created_at` as Unix seconds.
    pub opened_at: i64,
    /// `updated_at` as Unix seconds; only set for a terminal agent status.
    pub closed_at: Option<i64>,
    pub active: bool,
}

/// One `CursorPage<Subagent>` page.
///
/// Serialized by hand so one `has_more` value is emitted under both the
/// official snake_case name and the camelCase name the rest of the gateway
/// uses, matching [`crate::domain::TurnPage`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SubagentPage {
    pub object: String,
    pub data: Vec<OfficialSubagent>,
    pub has_more: bool,
    /// Id of the last subagent in page order; `None` when the page is complete.
    #[serde(rename = "nextCursor", default)]
    pub next_cursor: Option<String>,
}

impl SubagentPage {
    #[must_use]
    pub fn new(data: Vec<OfficialSubagent>, has_more: bool, next_cursor: Option<String>) -> Self {
        Self {
            object: "list".to_owned(),
            data,
            has_more,
            next_cursor,
        }
    }
}

impl Serialize for SubagentPage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut page = serializer.serialize_struct("SubagentPage", 5)?;
        page.serialize_field("object", &self.object)?;
        page.serialize_field("data", &self.data)?;
        page.serialize_field("has_more", &self.has_more)?;
        page.serialize_field("hasMore", &self.has_more)?;
        page.serialize_field("nextCursor", &self.next_cursor)?;
        page.end()
    }
}
