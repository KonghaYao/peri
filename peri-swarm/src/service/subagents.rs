//! Pure projection of child-thread records into official `Subagent` objects.
//!
//! The session store returns the whole root tree (including the root itself),
//! ordered by `created_at ASC`. This module removes the root, applies the
//! official `(opened_at, id)` cursor order, and maps each record with the
//! owning session's public identity. It never touches `SessionResources`, so
//! both the actor service and the in-memory fake share one rule.

use serde_json::json;

use crate::domain::{
    ErrorCode, ErrorKind, ItemOrder, OfficialSubagent, SubagentPage, SubagentRecord,
    SubagentsWindow, SwarmError, SUBAGENT_OBJECT,
};

/// Map one neutral record to the official object.
///
/// A direct child (`parent_thread_id == root_thread_id`) is parented by the
/// single root agent, so the official id is `"root"`; a nested child keeps its
/// parent thread id.
#[must_use]
pub fn subagent_from(
    record: &SubagentRecord,
    logical_id: &str,
    root_thread_id: &str,
) -> OfficialSubagent {
    let parent_agent_id = match record.parent_thread_id.as_deref() {
        Some(parent) if parent != root_thread_id => parent.to_owned(),
        _ => "root".to_owned(),
    };
    OfficialSubagent {
        id: record.id.clone(),
        object: SUBAGENT_OBJECT.to_owned(),
        session_id: logical_id.to_owned(),
        parent_agent_id,
        name: record.name.clone(),
        instructions: None,
        opened_at: record.opened_at,
        closed_at: record.closed_at,
        status: if record.active { "active" } else { "closed" }.to_owned(),
    }
}

/// One `subagents.list` page.
///
/// The root thread is never a subagent. `after` is exclusive in the selected
/// order and must name a subagent of this session; anything else is
/// `400 invalid_query` rather than a silently empty page.
pub fn subagent_page(
    records: Vec<SubagentRecord>,
    logical_id: &str,
    root_thread_id: &str,
    window: &SubagentsWindow,
) -> Result<SubagentPage, SwarmError> {
    if window.limit == 0 || window.limit > SubagentsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query(format!(
            "limit must be within 1..={}",
            SubagentsWindow::MAX_LIMIT
        ))
        .with_details(json!({ "field": "limit", "received": window.limit })));
    }
    let mut ordered: Vec<SubagentRecord> = records
        .into_iter()
        .filter(|record| record.id != root_thread_id)
        .collect();
    ordered.sort_by(|left, right| {
        left.opened_at
            .cmp(&right.opened_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    if window.order == ItemOrder::Desc {
        ordered.reverse();
    }
    let start = match window.after.as_deref() {
        Some(after) => ordered
            .iter()
            .position(|record| record.id == after)
            .map(|position| position + 1)
            .ok_or_else(|| {
                SwarmError::invalid_query("after must name a subagent of this session")
                    .with_details(json!({ "field": "after", "received": after }))
            })?,
        None => 0,
    };
    let remaining = ordered.len().saturating_sub(start);
    let limit = window.limit as usize;
    let page = &ordered[start..start + remaining.min(limit)];
    let has_more = remaining > page.len();
    let next_cursor = has_more
        .then(|| page.last().map(|record| record.id.clone()))
        .flatten();
    let data = page
        .iter()
        .map(|record| subagent_from(record, logical_id, root_thread_id))
        .collect();
    Ok(SubagentPage::new(data, has_more, next_cursor))
}

/// `subagents.retrieve`: only a child of this root is visible.
pub fn subagent_by_id(
    records: Vec<SubagentRecord>,
    logical_id: &str,
    root_thread_id: &str,
    subagent_id: &str,
) -> Result<OfficialSubagent, SwarmError> {
    records
        .iter()
        .find(|record| record.id == subagent_id && record.id != root_thread_id)
        .map(|record| subagent_from(record, logical_id, root_thread_id))
        .ok_or_else(|| {
            SwarmError::new(
                ErrorKind::NotFound,
                ErrorCode::UnknownAgent,
                "unknown subagent for this session",
            )
            .with_details(json!({ "logicalId": logical_id, "subagentId": subagent_id }))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, parent: &str, opened_at: i64) -> SubagentRecord {
        SubagentRecord {
            id: id.to_owned(),
            parent_thread_id: Some(parent.to_owned()),
            name: Some(format!("name {id}")),
            opened_at,
            closed_at: None,
            active: true,
        }
    }

    fn window(order: ItemOrder, after: Option<&str>, limit: u32) -> SubagentsWindow {
        SubagentsWindow {
            order,
            after: after.map(str::to_owned),
            limit,
        }
    }

    #[test]
    fn subagent_page_excludes_root_and_breaks_ties_by_id() {
        let records = vec![
            record("thread-root", "", 0),
            record("thread-b", "thread-root", 10),
            record("thread-a", "thread-root", 10),
        ];
        let ascending = subagent_page(
            records.clone(),
            "agent",
            "thread-root",
            &window(ItemOrder::Asc, None, 20),
        )
        .expect("page");
        let ids: Vec<_> = ascending.data.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["thread-a", "thread-b"]);
        assert!(!ascending.has_more);
        assert!(ascending.next_cursor.is_none());

        let descending = subagent_page(
            records,
            "agent",
            "thread-root",
            &window(ItemOrder::Desc, None, 20),
        )
        .expect("page");
        let ids: Vec<_> = descending
            .data
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["thread-b", "thread-a"]);
    }

    #[test]
    fn subagent_page_cursor_is_exclusive_and_must_exist() {
        let records = vec![
            record("thread-a", "thread-root", 1),
            record("thread-b", "thread-root", 2),
            record("thread-c", "thread-root", 3),
        ];
        let page = subagent_page(
            records.clone(),
            "agent",
            "thread-root",
            &window(ItemOrder::Desc, Some("thread-c"), 1),
        )
        .expect("page");
        assert_eq!(page.data[0].id, "thread-b");
        assert!(page.has_more);
        assert_eq!(page.next_cursor.as_deref(), Some("thread-b"));

        let missing = subagent_page(
            records,
            "agent",
            "thread-root",
            &window(ItemOrder::Asc, Some("thread-root"), 20),
        )
        .expect_err("the root is not a cursor member");
        assert_eq!(missing.code(), ErrorCode::InvalidQuery);
    }

    #[test]
    fn direct_children_are_parented_by_root_and_nested_keep_the_parent() {
        let direct = subagent_from(
            &record("thread-a", "thread-root", 10),
            "agent",
            "thread-root",
        );
        assert_eq!(direct.parent_agent_id, "root");
        assert_eq!(direct.session_id, "agent");
        assert_eq!(direct.object, "agent.session.subagent");
        assert!(direct.instructions.is_none());

        let nested = subagent_from(&record("thread-b", "thread-a", 20), "agent", "thread-root");
        assert_eq!(nested.parent_agent_id, "thread-a");
    }
}
