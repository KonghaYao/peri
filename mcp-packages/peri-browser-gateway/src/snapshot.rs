//! Text snapshots with stable uids.
//!
//! The snapshot is built from `Accessibility.getFullAXTree`: every rendered node
//! with a `backendDOMNodeId` gets a uid (`<snapshotNonce>_<n>`), and the page keeps
//! `uid -> backendNodeId`. Interaction tools only accept uids from the latest
//! snapshot; any navigation invalidates the mapping (README §7).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::error::BrowserError;

/// Upper bound on rendered AX nodes; a huge document must still produce a tool
/// response, so the tail is cut with a note.
pub const MAX_SNAPSHOT_NODES: usize = 4000;

/// Mapping kept between snapshots for one page.
#[derive(Debug, Clone, Default)]
pub struct SnapshotBook {
    generation: u64,
    valid: bool,
    pub(crate) document: Option<String>,
    uids: HashMap<String, i64>,
}

impl SnapshotBook {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Starts a new generation and drops the old uid mapping.
    pub fn invalidate(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.valid = false;
        self.uids.clear();
        self.document = None;
    }

    /// Stores the result of a successful snapshot.
    pub fn store(&mut self, snapshot: &BuiltSnapshot) {
        self.generation = snapshot.generation;
        self.valid = true;
        self.uids = snapshot.uids.clone();
    }

    /// Maps a uid from the latest snapshot to its backend node id.
    ///
    /// # Errors
    ///
    /// [`BrowserError::StaleUid`] when the page navigated or the snapshot was
    /// never taken, or when the uid is not in it.
    pub fn resolve(&self, uid: &str) -> Result<i64, BrowserError> {
        if !self.is_valid() {
            return Err(BrowserError::StaleUid(uid.to_owned()));
        }
        self.uids
            .get(uid)
            .copied()
            .ok_or_else(|| BrowserError::StaleUid(uid.to_owned()))
    }

    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.uids.len()
    }

    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uids.is_empty()
    }
}

/// Rendered snapshot text plus the uid mapping it introduced.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltSnapshot {
    pub text: String,
    pub uids: HashMap<String, i64>,
    pub generation: u64,
    pub node_count: usize,
}

struct AxNode<'a> {
    id: &'a str,
    role: &'a str,
    name: &'a str,
    value: Option<&'a str>,
    backend_node_id: Option<i64>,
    ignored: bool,
    children: Vec<&'a str>,
    properties: Vec<(String, String)>,
}

/// Builds the snapshot text from the flat `Accessibility.getFullAXTree` nodes.
#[must_use]
pub fn build_ax_snapshot(nodes: &[Value], generation: u64) -> BuiltSnapshot {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut parsed = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Some(id) = node.get("nodeId").and_then(Value::as_str) else {
            continue;
        };
        let role = string_field(node, "role");
        let name = string_field(node, "name");
        let value = node
            .get("value")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let ignored = node
            .get("ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let backend_node_id = node
            .get("backendDOMNodeId")
            .and_then(Value::as_i64)
            .filter(|id| *id > 0);
        let children = node
            .get("childIds")
            .and_then(Value::as_array)
            .map(|children| {
                children
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let properties = node
            .get("properties")
            .and_then(Value::as_array)
            .map(|properties| {
                properties
                    .iter()
                    .filter_map(|property| {
                        let name = property.get("name")?.as_str()?.to_owned();
                        let value = property.get("value")?;
                        let rendered = value
                            .get("value")
                            .map_or_else(String::new, |value| value.to_string());
                        Some((name, rendered))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        parsed.push(AxNode {
            id,
            role,
            name,
            value,
            backend_node_id,
            ignored,
            children,
            properties,
        });
    }

    let index: HashMap<&str, usize> = parsed
        .iter()
        .enumerate()
        .map(|(position, node)| (node.id, position))
        .collect();
    let mut child_ids: HashSet<&str> = HashSet::new();
    for node in &parsed {
        child_ids.extend(node.children.iter().copied());
    }
    let roots: Vec<&str> = parsed
        .iter()
        .filter(|node| !child_ids.contains(node.id))
        .map(|node| node.id)
        .collect();

    let mut text = String::new();
    let mut uids = HashMap::new();
    let mut visited = HashSet::new();
    let mut counter = 0usize;
    let mut rendered = 0usize;
    let mut truncated = false;
    for root in roots {
        walk(
            root,
            &index,
            &parsed,
            0,
            &nonce,
            &mut counter,
            &mut uids,
            &mut text,
            &mut visited,
            &mut rendered,
            &mut truncated,
        );
        if truncated {
            break;
        }
    }

    if text.is_empty() {
        text.push_str("(empty accessibility tree)");
    }
    if truncated {
        text.push_str("\n... snapshot truncated");
    }
    BuiltSnapshot {
        text,
        uids,
        generation,
        node_count: rendered,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    id: &str,
    index: &HashMap<&str, usize>,
    nodes: &[AxNode<'_>],
    depth: usize,
    nonce: &str,
    counter: &mut usize,
    uids: &mut HashMap<String, i64>,
    text: &mut String,
    visited: &mut HashSet<String>,
    rendered: &mut usize,
    truncated: &mut bool,
) {
    if *truncated || !visited.insert(id.to_owned()) {
        return;
    }
    let Some(node) = index.get(id).map(|position| &nodes[*position]) else {
        return;
    };
    let child_depth = if node.ignored {
        depth
    } else {
        if *rendered >= MAX_SNAPSHOT_NODES {
            *truncated = true;
            return;
        }
        *rendered += 1;
        let uid = node.backend_node_id.map(|backend| {
            let uid = format!("{nonce}_{counter}");
            *counter += 1;
            uids.insert(uid.clone(), backend);
            uid
        });
        let line = render_line(node, uid.as_deref(), depth);
        text.push_str(&line);
        text.push('\n');
        depth + 1
    };
    for child in &node.children {
        walk(
            child,
            index,
            nodes,
            child_depth,
            nonce,
            counter,
            uids,
            text,
            visited,
            rendered,
            truncated,
        );
        if *truncated {
            return;
        }
    }
}

fn render_line(node: &AxNode<'_>, uid: Option<&str>, depth: usize) -> String {
    let mut line = "  ".repeat(depth);
    if let Some(uid) = uid {
        line.push_str("uid=");
        line.push_str(uid);
        line.push(' ');
    }
    line.push_str(if node.role.is_empty() {
        "generic"
    } else {
        node.role
    });
    if !node.name.is_empty() {
        line.push(' ');
        line.push('"');
        line.push_str(&escape(node.name));
        line.push('"');
    }
    if let Some(value) = node.value {
        line.push_str(" value=\"");
        line.push_str(&escape(value));
        line.push('"');
    }
    for (name, value) in &node.properties {
        let include = match name.as_str() {
            "disabled" | "checked" | "focused" | "expanded" | "required" | "readonly" => {
                value == "true"
            }
            "level" | "valuetext" => !value.is_empty(),
            _ => false,
        };
        if include {
            line.push(' ');
            line.push_str(name);
            if name == "level" || name == "valuetext" {
                line.push('=');
                line.push('"');
                line.push_str(&escape(value));
                line.push('"');
            }
        }
    }
    line
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn string_field<'a>(node: &'a Value, key: &str) -> &'a str {
    node.get(key)
        .and_then(|field| field.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
