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

const MAX_ITEM_PAGE_READS: usize = 100;
const MAX_SELECTED_RAW_ROWS: usize = 2000;

pub(crate) async fn item_page(
    store: &dyn SwarmStore,
    logical_id: &LogicalId,
    window: ItemsWindow,
    last_item_seq: u64,
) -> Result<ItemPage, SwarmError> {
    let mut cursor = resolve_window_after(store, logical_id, &window).await?;
    let logical_id = logical_id.as_str();
    let mut items = Vec::new();
    // 组会因并发追加而改变最大 seq 并被后续页再次选中；按 seq 保持集合语义。
    let mut seen = std::collections::BTreeSet::new();
    let mut reads = 0;
    let (mut data, next_cursor, has_more) = loop {
        let page = store
            .swarm_item_page(logical_id, window.order, cursor, window.limit)
            .await
            .map_err(|error| store_failure("item_page", error))?;
        reads += 1;
        let rows: Vec<_> = page.rows.iter().map(|row| row.to_view()).collect();
        // 只有尚未纳入的 seq 计入 2000 行预算；重复帧不重复计数。
        let new_rows: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|row| row.seq)
            .filter(|seq| !seen.contains(seq))
            .collect();
        if items.len() + new_rows.len() > MAX_SELECTED_RAW_ROWS {
            return Err(SwarmError::items_scan_limit());
        }
        // 每个存储页独立投影；前面已经确认没有投影的页不再消耗回扫预算。
        let data = project_page(store, logical_id, &page, &rows).await?;
        for row in rows {
            if seen.insert(row.seq) {
                items.push(row);
            }
        }
        if !data.is_empty() || !page.has_more {
            break (data, page.next_cursor, page.has_more);
        }
        let next = page
            .next_cursor
            .filter(|&next| {
                cursor.is_none_or(|previous| match window.order {
                    ItemOrder::Asc => next > previous,
                    ItemOrder::Desc => next < previous,
                })
            })
            .ok_or_else(SwarmError::items_scan_limit)?;
        if window.order == ItemOrder::Asc && next >= last_item_seq {
            // `last_item_seq` 是请求开始时的观察上界；之后追加的条目不属于本次请求。
            break (data, page.next_cursor, page.has_more);
        }
        if reads == MAX_ITEM_PAGE_READS {
            return Err(SwarmError::items_scan_limit());
        }
        cursor = Some(next);
    };
    // 各页按组最大 seq 选入，多帧组的早期帧可能和其他页交错。
    items.sort_unstable_by_key(|row| row.seq);
    if window.order == ItemOrder::Desc {
        items.reverse();
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
    // 按组最大 seq 的顺序取投影；每个 item_id 只会从映射中移除一次。
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
