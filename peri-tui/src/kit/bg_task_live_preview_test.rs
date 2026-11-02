use super::*;
use crate::kit::tui_render_unit::TuiAssistantBubble;

fn detail(index: usize, status: BgLiveStatus) -> BgLiveDetail {
    let mut detail = BgLiveDetail {
        status,
        preview_updated: index as u64,
        ..Default::default()
    };
    detail
        .nested_units
        .push_back(TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                text: "x".repeat(64 * 1024),
                reasoning: None,
                message_id: None,
                started_at: None,
                duration_ms: None,
                content_hash: 0,
            }
            .into(),
        ));
    detail
}

/// [回归测试] 多个仍在运行的任务也共享展示预算，身份和真实执行不受淘汰影响。
#[test]
fn active_tasks_share_a_body_budget_without_losing_control_identity() {
    let mut map = HashMap::new();
    for index in 0..100 {
        map.insert(
            format!("task-{index}"),
            detail(index, BgLiveStatus::Running),
        );
    }
    enforce(&mut map);
    assert_eq!(map.len(), 100);
    assert!(
        map.values()
            .all(|detail| detail.status == BgLiveStatus::Running)
    );
    assert!(
        map.values()
            .map(BgLiveDetail::preview_reserved)
            .sum::<usize>()
            <= PREVIEW_BYTES
    );
    assert!(map["task-0"].preview_omitted);
    assert!(!map["task-99"].nested_units.is_empty());
}

/// [回归测试] map 退场不是释放确认；旧 im 快照释放后 Weak 才应失效。
#[test]
fn eviction_does_not_claim_that_a_published_snapshot_has_been_freed() {
    let first = detail(0, BgLiveStatus::Running);
    let snapshot = first.nested_units.clone();
    let TuiRenderUnit::TuiAssistantBubble(bubble) = &snapshot[0] else {
        unreachable!()
    };
    let weak = std::sync::Arc::downgrade(bubble);
    let mut map = HashMap::from([("first".to_string(), first)]);
    for index in 1..100 {
        map.insert(
            format!("task-{index}"),
            detail(index, BgLiveStatus::Running),
        );
    }
    enforce(&mut map);
    assert!(map["first"].nested_units.is_empty());
    assert!(
        weak.upgrade().is_some(),
        "外部快照仍拥有正文，不得提前记为已释放"
    );
    drop(snapshot);
    assert!(
        weak.upgrade().is_none(),
        "最后一个真实强引用释放后正文才退场"
    );
}

#[test]
fn utf8_tail_and_large_input_capacity_are_bounded_before_copying() {
    let mut omitted = false;
    let mut value = "开始".repeat(STREAM_BYTES);
    append(&mut value, &"尾🙂".repeat(STREAM_BYTES), &mut omitted);
    assert!(omitted);
    assert!(value.len() <= STREAM_BYTES);
    assert!(value.capacity() <= STREAM_BYTES);
    assert!(value.ends_with("尾🙂"));
    let mut spare = String::with_capacity(1024 * 1024);
    spare.push_str("short");
    assert_eq!(text(&spare, FIELD_BYTES, &mut omitted).capacity(), 5);
}

#[test]
fn json_container_storage_is_not_mistaken_for_small_preview_text() {
    let array = serde_json::Value::Array(vec![serde_json::Value::Null; FIELD_BYTES]);
    let mut omitted = false;
    let copy = raw_input(&array, &mut omitted);
    assert!(copy.is_null());
    assert!(omitted);
    assert_eq!(array.as_array().unwrap().len(), FIELD_BYTES);
}
