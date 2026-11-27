//! 在现有原始条目页上生成官方 `AgentSessionItem` 视图。

use std::collections::{btree_map::Entry, BTreeMap};

use super::{resolve_window_after, turn_at_cursor, user_opens_turn, TURN_LOOKBACK_LIMIT};
use crate::domain::official_item::{
    map_group, GroupClass, ItemGroup, OfficialSessionItem, TurnContext,
};
use crate::domain::{
    turn_id_for_item, ErrorCode, ItemKind, ItemOrder, ItemPage, ItemView, ItemsWindow, LogicalId,
    SwarmError, TurnStatus,
};
use crate::store::{store_failure, SwarmItemPage, SwarmStore};

/// 单次条目投影回扫最多读取的原始行页数。
const MAX_TURN_LOOKBACK_PAGES: usize = 10;

/// 每个原始页投影允许的 `turn_at_cursor` 回扫次数上限。
///
/// 历史多行（item 粒度之前）里根消息流与推理流/子 agent 流按 `seq` 交错，
/// 页内每个缺口都要回扫到 turn opener。超过上限的缺口放弃精确归属：
/// 该组及其后续组只留在 `items`，不把猜测写进官方 `turn_id`。
const MAX_TURN_LOOKBACKS: usize = 8;

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
    let (next_cursor, has_more) = (page.next_cursor, page.has_more);
    let mut data = project_page(store, logical_id, &page, &items).await?;
    let mut scan = page;
    // 官方 SDK 在 data 为空时不会用 nextCursor 继续；只前探 data，不移动原始页。
    while data.is_empty() && scan.has_more {
        let Some(cursor) = scan.next_cursor else {
            break;
        };
        if window.order == ItemOrder::Asc && cursor >= last_item_seq {
            // 升序不追着并发追加的条目继续扫描。
            break;
        }
        scan = store
            .swarm_item_page(logical_id, window.order, Some(cursor), window.limit)
            .await
            .map_err(|error| store_failure("item_page", error))?;
        let rows: Vec<_> = scan.rows.iter().map(|row| row.to_view()).collect();
        data = project_page(store, logical_id, &scan, &rows).await?;
    }
    reconcile_calls(&mut data, window.order);
    Ok(ItemPage::new(
        logical_id,
        items,
        data,
        next_cursor,
        last_item_seq,
        has_more,
        window.order,
    ))
}

async fn project_page(
    store: &dyn SwarmStore,
    logical_id: &str,
    page: &SwarmItemPage,
    rows: &[ItemView],
) -> Result<Vec<OfficialSessionItem>, SwarmError> {
    let mut mapped = project_items(store, logical_id, rows).await?;
    // store 已按条目的最大 seq 排列；投影所选的完成行不一定是该最大 seq。
    Ok(page
        .items
        .iter()
        .filter_map(|row| mapped.remove(row.item_id.as_str()))
        .collect())
}

async fn project_items(
    store: &dyn SwarmStore,
    logical_id: &str,
    items: &[ItemView],
) -> Result<BTreeMap<String, OfficialSessionItem>, SwarmError> {
    let Some(min_seq) = items.iter().map(|row| row.seq).min() else {
        return Ok(BTreeMap::new());
    };
    let mut has_output = false;
    let mut current = None;
    let mut statuses = BTreeMap::new();
    // 多帧条目跨过未入页的条目时，原始行的 seq 可能不连续。缺口中也可能有开轮或完成帧。
    let mut seqs: Vec<_> = items.iter().map(|row| row.seq).collect();
    seqs.sort_unstable();
    // 相同边界只回扫一次：缺口起点去重后再按条目顺序消费。
    let seed_bounds: Vec<_> = (min_seq > 1)
        .then_some(min_seq)
        .into_iter()
        .chain(
            seqs.windows(2)
                .filter_map(|pair| (pair[1] - pair[0] > 1).then_some(pair[1])),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut seed_at = seed_bounds.into_iter().peekable();
    let mut lookbacks = 0usize;

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
        let mut seed = None;
        while let Some(seq) = seed_at.next_if(|&seq| seq <= group.selected.seq) {
            seed = Some(seq - 1);
        }
        if let Some(up_to) = seed {
            if lookbacks == MAX_TURN_LOOKBACKS {
                tracing::warn!(
                    logical_id,
                    max = MAX_TURN_LOOKBACKS,
                    "swarm items turn seeding degraded to items-only"
                );
                break;
            }
            lookbacks += 1;
            let mut persisted = BTreeMap::new();
            let previous = match turn_at_cursor(
                store,
                logical_id,
                up_to,
                &mut persisted,
                Some(MAX_TURN_LOOKBACK_PAGES),
            )
            .await
            {
                Ok(previous) => previous,
                Err(error)
                    if error.code() == ErrorCode::UpstreamUnavailable
                        && error.details()["operation"] == TURN_LOOKBACK_LIMIT =>
                {
                    // 尚未验证的缺口可能影响此组及其后的条目。
                    break;
                }
                Err(error) => return Err(error),
            };
            has_output = previous
                .as_ref()
                .is_some_and(|turn| turn.completed_at.is_some());
            current = previous.map(|turn| turn.turn_id);
            statuses.extend(
                persisted
                    .into_iter()
                    .map(|(id, row)| (id, Some(row.status))),
            );
        }
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
        let status = match group.class {
            GroupClass::UnfinishedAssistant => {
                turn_status(store, logical_id, &mut statuses, turn_id).await?
            }
            // 调用行自身没有终态：同一 turn 已终止时按 `incomplete` 兜底。
            _ if group.selected.kind == ItemKind::ToolCall => {
                turn_status(store, logical_id, &mut statuses, turn_id).await?
            }
            _ => None,
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
