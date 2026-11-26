//! 在现有原始条目页上生成官方 `AgentSessionItem` 视图。

use std::collections::{btree_map::Entry, BTreeMap};

use super::{resolve_window_after, turn_at_cursor, user_opens_turn};
use crate::domain::official_item::{
    map_group, GroupClass, ItemGroup, OfficialSessionItem, TurnContext,
};
use crate::domain::{
    turn_id_for_item, ItemOrder, ItemPage, ItemView, ItemsWindow, LogicalId, SwarmError, TurnStatus,
};
use crate::store::{store_failure, SwarmStore};

pub(crate) async fn item_page(
    store: &dyn SwarmStore,
    logical_id: &LogicalId,
    window: ItemsWindow,
    last_item_seq: u64,
) -> Result<ItemPage, SwarmError> {
    let after = resolve_window_after(store, logical_id, &window).await?;
    let logical_id = logical_id.as_str();
    let page = store
        .swarm_item_page(logical_id, window.order, after, window.limit)
        .await
        .map_err(|error| store_failure("item_page", error))?;
    let items: Vec<_> = page.rows.iter().map(|row| row.to_view()).collect();
    let mut mapped = project_items(store, logical_id, &items).await?;
    // store 已按条目的最大 seq 排列；投影所选的完成行不一定是该最大 seq。
    let mut data: Vec<_> = page
        .items
        .iter()
        .filter_map(|row| mapped.remove(row.item_id.as_str()))
        .collect();
    reconcile_calls(&mut data, window.order);
    Ok(ItemPage::new(
        logical_id,
        items,
        data,
        page.next_cursor,
        last_item_seq,
        page.has_more,
        window.order,
    ))
}

async fn project_items(
    store: &dyn SwarmStore,
    logical_id: &str,
    items: &[ItemView],
) -> Result<BTreeMap<String, OfficialSessionItem>, SwarmError> {
    let Some(min_seq) = items.iter().map(|row| row.seq).min() else {
        return Ok(BTreeMap::new());
    };
    let mut persisted = BTreeMap::new();
    let previous = if min_seq > 1 {
        turn_at_cursor(store, logical_id, min_seq - 1, &mut persisted).await?
    } else {
        None
    };
    let mut has_output = previous
        .as_ref()
        .is_some_and(|turn| turn.completed_at.is_some());
    let mut current = previous.map(|turn| turn.turn_id);
    let mut statuses = persisted
        .into_iter()
        .map(|(id, row)| (id, Some(row.status)))
        .collect();

    let mut rows_by_item: BTreeMap<&str, Vec<&ItemView>> = BTreeMap::new();
    for row in items {
        rows_by_item.entry(&row.item_id).or_default().push(row);
    }
    let mut groups: Vec<_> = rows_by_item
        .into_values()
        .filter_map(ItemGroup::new)
        .collect();
    groups.sort_unstable_by_key(|group| group.selected.seq);

    let mut mapped = BTreeMap::new();
    for group in groups {
        let id = &group.selected.item_id;
        if group.class == GroupClass::User {
            let candidate = turn_id_for_item(id);
            // 有明确边界时无需查询持久 turn；否则只有持久 opener 能把 steer 判为新 turn。
            let existing = if current.is_some() && !has_output {
                turn_status(store, logical_id, &mut statuses, &candidate)
                    .await?
                    .is_some()
            } else {
                false
            };
            if user_opens_turn(current.is_some(), has_output, existing) {
                current = Some(candidate);
                has_output = false;
            }
        }
        let Some(turn_id) = current.as_deref() else {
            continue;
        };
        if group.class == GroupClass::CompleteAssistant {
            // 边界不取决于 payload 是否能进入 data。
            has_output = true;
        }
        let status = if group.class == GroupClass::UnfinishedAssistant {
            turn_status(store, logical_id, &mut statuses, turn_id).await?
        } else {
            None
        };
        if let Some(item) = map_group(
            &group,
            Some(TurnContext {
                id: turn_id,
                status,
            }),
        ) {
            mapped.insert(id.clone(), item);
        }
    }
    Ok(mapped)
}

async fn turn_status(
    store: &dyn SwarmStore,
    logical_id: &str,
    statuses: &mut BTreeMap<String, Option<TurnStatus>>,
    turn_id: &str,
) -> Result<Option<TurnStatus>, SwarmError> {
    match statuses.entry(turn_id.to_owned()) {
        Entry::Occupied(entry) => Ok(*entry.get()),
        Entry::Vacant(entry) => {
            let status = store
                .swarm_get_turn(logical_id, turn_id)
                .await
                .map_err(|error| store_failure("get_turn", error))?
                .map(|row| row.status);
            Ok(*entry.insert(status))
        }
    }
}

fn reconcile_calls(data: &mut [OfficialSessionItem], order: ItemOrder) {
    let mut outputs = BTreeMap::new();
    let mut record_output = |item: &OfficialSessionItem| {
        if let OfficialSessionItem::FunctionCallOutput {
            call_id, status, ..
        } = item
        {
            outputs.entry(call_id.clone()).or_insert(*status);
        }
    };
    match order {
        ItemOrder::Asc => data.iter().rev().for_each(&mut record_output),
        ItemOrder::Desc => data.iter().for_each(&mut record_output),
    }
    for item in data.iter_mut() {
        if let OfficialSessionItem::FunctionCall {
            call_id, status, ..
        } = item
        {
            if let Some(output_status) = outputs.get(call_id) {
                *status = *output_status;
            }
        }
    }
}

#[cfg(test)]
#[path = "official_items_test.rs"]
mod tests;
