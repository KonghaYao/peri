use super::*;

/// 测试限额：单条原文 512、单批原文 4096、单批编码 2500、条数 10。
fn limits() -> BatchLimits {
    BatchLimits {
        max_event_bytes: 512,
        max_batch_bytes: 4096,
        max_encoded_bytes: 2500,
        max_events: 10,
    }
}

fn measured(raw_bytes: u64, encoded_bytes: u64) -> PreparedEvent {
    PreparedEvent::Measured(EventMeasure {
        raw_bytes,
        encoded_bytes,
    })
}

/// 与 emitter 同构的精确正文模型：信封 + Σ元素 + 分隔符。
/// 单条独立计量 = 信封 + 元素，故批次合计先扣掉每条的信封再补一次。
const ENVELOPE: u64 = 12;
const SEPARATOR: u64 = 1;

fn exact_batch_bytes(batch: &BatchWindow) -> u64 {
    let elements = batch.len() as u64;
    // 模型前提：每条单条计量都含一份信封（encoded_bytes ≥ ENVELOPE）；低于该前提的合成
    // 数值不是本模型的输入，用显式断言代替整数下溢，避免把夹具错误伪装成产品缺陷。
    assert!(
        batch.encoded_bytes >= elements * ENVELOPE,
        "synthetic measurement {encoded} is below the envelope model for {elements} elements",
        encoded = batch.encoded_bytes
    );
    let element_bytes = batch.encoded_bytes - elements * ENVELOPE;
    ENVELOPE + element_bytes + elements.saturating_sub(1) * SEPARATOR
}

/// 批次覆盖的下标是否恰好是 `expected` 且无重复。
fn covered_indices(plan: &SplitPlan) -> Vec<usize> {
    let mut indices: Vec<usize> = plan.batches.iter().flat_map(|b| b.range.clone()).collect();
    indices.sort_unstable();
    indices
}

#[test]
fn empty_input_plans_nothing() {
    let plan = split(&[], &limits()).unwrap();
    assert!(plan.batches.is_empty());
    assert!(plan.rejected.is_empty());
    assert_eq!(plan.deferred, None);
    assert_eq!(plan.planned_len(0), 0);
    assert_eq!(plan.accounted_len(), 0);
}

#[test]
fn single_event_exactly_at_the_limits_stays_a_batch_of_one() {
    // 原文恰好等于 max_event_bytes、编码恰好等于 max_encoded_bytes：上限含等号。
    let plan = split(&[measured(512, 2500)], &limits()).unwrap();
    assert_eq!(plan.batches.len(), 1);
    assert_eq!(plan.batches[0].range, 0..1);
    assert_eq!(plan.batches[0].raw_bytes, 512);
    assert_eq!(plan.batches[0].encoded_bytes, 2500);
    assert!(!plan.batches[0].is_empty());
    assert_eq!(plan.accounted_len(), 1);

    // 再加一条即拆批：前一批保持在上限内，不越限吸收。
    let plan = split(&[measured(512, 2500), measured(1, 1)], &limits()).unwrap();
    assert_eq!(plan.batches.len(), 2);
    assert_eq!(plan.batches[0].range, 0..1);
    assert_eq!(plan.batches[1].range, 1..2);
}

#[test]
fn encoded_expansion_splits_without_losing_events() {
    // 单条原文 600 必须仍在单条原文上限内，否则事件在拆批前就被精确拒收（另有用例覆盖）。
    let limits = BatchLimits {
        max_event_bytes: 600,
        ..limits()
    };
    // 每条独立编码 1212 字节（信封 12 + 元素 1200），原文只有 600：
    // 按原文 6 条合计 3600 ≤ 4096 本可成一批，编码合计 7272 > 2500 必须拆批。
    let prepared = vec![measured(600, 1212); 6];
    let plan = split(&prepared, &limits).unwrap();

    assert_eq!(plan.batches.len(), 3);
    assert_eq!(covered_indices(&plan), (0..6).collect::<Vec<_>>());
    assert_eq!(plan.accounted_len(), 6);
    assert_eq!(plan.planned_len(6), 6);
    assert!(plan.rejected.is_empty());

    for batch in &plan.batches {
        assert!(batch.raw_bytes <= 4096);
        assert!(batch.encoded_bytes <= 2500);
        // 实际正文（信封 + 元素 + 分隔符）同样在限额内，不是靠取整掩盖超限。
        assert!(exact_batch_bytes(batch) <= 2500);
    }
    // 拆批是编码维度导致的：相邻批次合并即越限（条数上限 10 不会阻止合并）。
    for pair in plan.batches.windows(2) {
        assert!(pair[0].encoded_bytes + pair[1].encoded_bytes > 2500);
        assert!(pair[0].len() + pair[1].len() <= 10);
    }
}

#[test]
fn oversized_encoded_span_is_rejected_while_the_rest_continues() {
    let huge = EventMeasure {
        raw_bytes: 500,
        encoded_bytes: 9_999_999,
    };
    let prepared = vec![
        measured(100, 1000),
        PreparedEvent::Oversized {
            measure: huge,
            reason: OversizeReason::Encoded { limit_bytes: 2500 },
        },
        measured(100, 1000),
    ];
    let plan = split(&prepared, &limits()).unwrap();

    assert_eq!(plan.batches.len(), 2);
    assert_eq!(plan.batches[0].range, 0..1);
    assert_eq!(plan.batches[1].range, 2..3);
    assert_eq!(covered_indices(&plan), vec![0, 2]);
    assert_eq!(plan.rejected.len(), 1);
    assert_eq!(plan.rejected[0].index, 1);
    // 计量完整保留：既没有截断 input，也没有伪造分片事件。
    assert_eq!(plan.rejected[0].measure, huge);
    assert_eq!(plan.accounted_len(), 3);
    assert!(matches!(
        plan.rejected[0].error(),
        LangfuseError::SingleSpanTooLarge {
            limit_bytes: 2500,
            encoded_bytes: 9_999_999
        }
    ));
}

#[test]
fn oversized_raw_span_maps_to_the_frozen_error() {
    let prepared = vec![
        PreparedEvent::Oversized {
            measure: EventMeasure {
                raw_bytes: 65_536,
                encoded_bytes: 900,
            },
            reason: OversizeReason::Raw { limit_bytes: 512 },
        },
        measured(10, 10),
    ];
    let plan = split(&prepared, &limits()).unwrap();
    assert_eq!(plan.batches[0].range, 1..2);
    assert!(matches!(
        plan.rejected[0].error(),
        LangfuseError::PayloadTooLarge { limit_bytes: 512 }
    ));
    assert_eq!(plan.rejected[0].measure.raw_bytes, 65_536);
}

#[test]
fn event_count_limit_splits_batches() {
    let mut limits = limits();
    limits.max_events = 2;
    let plan = split(&[measured(1, 1); 5], &limits).unwrap();
    let ranges: Vec<_> = plan.batches.iter().map(|b| b.range.clone()).collect();
    assert_eq!(ranges, vec![0..2, 2..4, 4..5]);
    assert_eq!(plan.accounted_len(), 5);
}

#[test]
fn raw_batch_limit_splits_before_the_encoded_limit() {
    let mut limits = limits();
    limits.max_batch_bytes = 1000;
    limits.max_encoded_bytes = 100_000;
    let plan = split(
        &[measured(400, 10), measured(400, 10), measured(400, 10)],
        &limits,
    )
    .unwrap();
    let ranges: Vec<_> = plan.batches.iter().map(|b| b.range.clone()).collect();
    assert_eq!(ranges, vec![0..2, 2..3]);
    assert_eq!(plan.batches[0].raw_bytes, 800);
}

#[test]
fn waiting_event_defers_the_remaining_input() {
    let prepared = vec![
        measured(100, 100),
        PreparedEvent::Waiting,
        measured(100, 100),
    ];
    let plan = split(&prepared, &limits()).unwrap();
    assert_eq!(plan.deferred, Some(1));
    assert_eq!(plan.planned_len(3), 1);
    assert_eq!(plan.accounted_len(), 1);
    assert_eq!(plan.batches.len(), 1);
    assert_eq!(plan.batches[0].range, 0..1);
    assert!(plan.rejected.is_empty());
}

#[test]
fn measured_item_over_the_encoded_limit_is_rejected_not_batched() {
    // 第二条用满足信封模型的计量（encoded ≥ ENVELOPE），使精确正文断言有实际内容。
    let prepared = vec![measured(100, 2501), measured(100, 1200)];
    let plan = split(&prepared, &limits()).unwrap();
    assert_eq!(plan.rejected.len(), 1);
    assert_eq!(plan.rejected[0].index, 0);
    assert_eq!(
        plan.rejected[0].reason,
        OversizeReason::Encoded { limit_bytes: 2500 }
    );
    assert_eq!(plan.batches.len(), 1);
    assert_eq!(plan.batches[0].range, 1..2);
    for batch in &plan.batches {
        assert!(exact_batch_bytes(batch) <= 2500);
    }
}

#[test]
fn inconsistent_limits_are_config_errors() {
    let no_events = BatchLimits {
        max_events: 0,
        ..limits()
    };
    assert!(matches!(
        split(&[], &no_events),
        Err(LangfuseError::Config(_))
    ));

    let no_encoded = BatchLimits {
        max_encoded_bytes: 0,
        ..limits()
    };
    assert!(matches!(
        split(&[], &no_encoded),
        Err(LangfuseError::Config(_))
    ));

    let narrow_batch = BatchLimits {
        max_batch_bytes: limits().max_event_bytes - 1,
        ..limits()
    };
    assert!(matches!(
        split(&[], &narrow_batch),
        Err(LangfuseError::Config(_))
    ));
}
