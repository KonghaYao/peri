//! Caller-declared MCP tools (`design §2.2`, §5.1).
//!
//! `POST /v1/agents/sessions` accepts the OpenAI Agents API MCP tool shape. The
//! HTTP layer normalizes that wire shape into these typed values; the service and
//! the store only ever see [`McpToolSpec`]. The canonical JSON written to
//! `swarm_sessions.mcp_json` is exactly the serialization of this type, so a
//! declaration read back from the store compares equal to the one that was
//! accepted (`design §4.3`).
//!
//! Normalization rules owned by this module:
//!
//! - `headers` is an ordered array of `{name,value}` pairs, stable-sorted by
//!   name. The object form (`{"name":"value"}`) and the array form therefore
//!   produce the same canonical value, and repeated names keep their relative
//!   order;
//! - `require_approval` and `allowed_tools` are preserved verbatim (string or
//!   object): S8a only records them, it never interprets or enforces them;
//! - unknown official extension fields are kept in [`McpToolSpec::extra`] and
//!   flattened back into the canonical JSON.
//!
//! S8a does **not** inject these declarations into ACP. Injection is S8a2's
//! scope (`design §5.1`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Tool entry discriminator. Stored so `mcp_json` holds the complete official
/// tool entry rather than an implied shape.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpToolKind {
    #[default]
    Mcp,
}

/// One MCP tool declared by the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpToolSpec {
    /// Always [`McpToolKind::Mcp`]; present in the canonical JSON.
    #[serde(rename = "type", default)]
    pub kind: McpToolKind,
    pub server_label: String,
    pub server_url: String,
    /// Normalized `headers` pairs (stable order: by name, then input order).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<McpHeader>,
    /// OpenAI `require_approval` value (`"always"`, `"never"`, or the object
    /// form), preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<Value>,
    /// OpenAI `allowed_tools` value (array of names or filter object),
    /// preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Value>,
    /// Official extension fields this contract does not interpret.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl McpToolSpec {
    #[must_use]
    pub fn new(server_label: impl Into<String>, server_url: impl Into<String>) -> Self {
        Self {
            kind: McpToolKind::Mcp,
            server_label: server_label.into(),
            server_url: server_url.into(),
            headers: Vec::new(),
            require_approval: None,
            allowed_tools: None,
            extra: Map::new(),
        }
    }

    /// Canonical `swarm_sessions.mcp_json` text.
    ///
    /// No declarations encode as `None`, which is also what pre-existing rows
    /// hold; both read back as an empty list.
    pub fn encode_declaration(tools: &[Self]) -> Result<Option<String>, serde_json::Error> {
        if tools.is_empty() {
            return Ok(None);
        }
        serde_json::to_string(tools).map(Some)
    }

    /// Decode `mcp_json`. `None` (old rows) is an empty declaration.
    pub fn decode_declaration(text: Option<&str>) -> Result<Vec<Self>, serde_json::Error> {
        match text {
            None => Ok(Vec::new()),
            Some(text) => serde_json::from_str(text),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpHeader {
    pub name: String,
    pub value: String,
}

#[cfg(test)]
#[path = "mcp_test.rs"]
mod tests;
