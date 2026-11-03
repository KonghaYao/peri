//! 驻留压力快照的失效条件与已提交取消路径。
//!
//! 作为 `resident_budget_test` 的子模块，复用其夹具、模型与阶段入口。

use super::*;
use crate::thread::ThreadMeta;

/// 快照绑定记录时的 limit：limit 变化后旧快照不得继续抑制 Full。
#[tokio::test]
async fn resident_snapshot_invalidates_when_limit_changes() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let (session, mut context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted);
    assert_eq!(model.request_count(), 1);

    // 旧快照记录 limit=RESIDUAL_LIMIT；改成不同 limit 后必须回到普通门控。
    context.compact.compact_config = Some(resident_config(RESIDUAL_LIMIT / 2));
    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        append_small_tool_exchange(&mut transcript, 0);
    }
    let second = run_stage(&context).await.unwrap();
    assert!(
        second.compacted,
        "limit 变化后旧快照失效，不得继续抑制 Full"
    );
    assert_eq!(model.request_count(), 2);
}

/// 快照绑定 transcript 实例与水印：重建/换出 transcript 后旧快照失效。
#[tokio::test]
async fn resident_snapshot_invalidates_when_transcript_replaced() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted);
    assert_eq!(model.request_count(), 1);

    // 用全新实例替换 transcript：实例身份变化且旧水印不存在。
    let replacement = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    {
        let handle = session.transcript();
        *handle.write() = replacement;
    }
    let second = run_stage(&context).await.unwrap();
    assert!(
        second.compacted,
        "transcript 重建后旧快照失效，不得继续抑制 Full"
    );
    assert_eq!(model.request_count(), 2);
}

/// Full 后摘要自身即可回收 own 且已超过极小 limit：后续小结果不得仅因绝对
/// own 超限而重复 Full；只有相对快照的增长超过完整 limit 才恢复。
#[tokio::test]
async fn resident_summary_growth_over_limit_does_not_repeat_full() {
    const LIMIT: u64 = 1_000;
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    // 初始内容必须超过预算以触发首次 Full。
    transcript.append(make_human(&"o".repeat(LIMIT as usize * 2)));
    // 摘要自身超过预算，复现「Full 后可回收 own 仍 > limit」。
    let model = RecordingModel::with_summary(false, &"s".repeat(LIMIT as usize * 2));
    let (session, context) =
        build_context(&bound, transcript, resident_config(LIMIT), model.clone());

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次超限必须 Full");
    assert_eq!(model.request_count(), 1);

    let baseline = ResidentHistoryPressure::from_transcript(LIMIT, &session.transcript().read())
        .reclaimable_own_reserved_bytes();
    assert!(
        baseline > LIMIT,
        "夹具要求 Full 后摘要自身已超过极小 limit，实际 {baseline}"
    );

    for round in 0..5 {
        {
            let handle = session.transcript();
            let mut transcript = handle.write();
            let id = format!("r{round}");
            transcript.append(make_small_tool_call(&id));
            transcript.append(make_small_tool_result(&id, "x"));
        }
        let output = run_stage(&context).await.unwrap();
        assert!(
            !output.compacted,
            "第 {round} 轮：小结果不得仅因绝对 own 超限触发 Full"
        );
        assert_eq!(model.request_count(), 1);
    }

    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.append(make_small_tool_call("growth"));
        transcript.append(make_small_tool_result(
            "growth",
            &"g".repeat(LIMIT as usize + 600),
        ));
    }
    let grown = ResidentHistoryPressure::from_transcript(LIMIT, &session.transcript().read())
        .reclaimable_own_reserved_bytes();
    assert!(
        grown > baseline.saturating_add(LIMIT),
        "可回收 own 相对快照增长必须超过完整 limit：baseline={baseline}, current={grown}"
    );

    let resumed = run_stage(&context).await.unwrap();
    assert!(resumed.compacted, "增长超过 limit 后应恢复 Full");
    assert_eq!(model.request_count(), 2);
}

/// 同 transcript 实例、同 watermark id 的内容替换（reconcile_to_transcript 路径）
/// 必须让快照失效并允许重新 Full。
#[tokio::test]
async fn resident_snapshot_invalidates_on_watermark_replacement() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted);
    assert_eq!(model.request_count(), 1);

    let watermark = {
        let handle = session.transcript();
        let transcript = handle.read();
        transcript.entries().last().expect("Full 后必须有摘要").id()
    };
    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.replace_by_id(
            BaseMessage::human("replaced watermark body").with_message_id(watermark),
        );
        transcript.append(make_small_tool_call("after_replace"));
        transcript.append(make_small_tool_result("after_replace", "z"));
    }

    let second = run_stage(&context).await.unwrap();
    assert!(
        second.compacted,
        "同 id 替换 watermark 后旧快照失效，应重新 Full"
    );
    assert_eq!(model.request_count(), 2);
}

/// 替换非 watermark 的 post-watermark 条目（同 id、同内容）不得让快照失效；
/// 这是有意取舍：只有 watermark 锚点变化才重新建立基线。
#[tokio::test]
async fn resident_snapshot_survives_non_watermark_replacement() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted);
    assert_eq!(model.request_count(), 1);

    let post_watermark_result = {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.append(make_small_tool_call("post_watermark"));
        let result = make_small_tool_result("post_watermark", "same body");
        let id = result.id();
        transcript.append(result);
        id
    };
    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.replace_by_id(
            make_small_tool_result("post_watermark", "same body")
                .with_message_id(post_watermark_result),
        );
        // 替换后再追加小结果，构成新的工作单元。
        transcript.append(make_small_tool_call("after_replace"));
        transcript.append(make_small_tool_result("after_replace", "x"));
    }

    let second = run_stage(&context).await.unwrap();
    assert!(
        !second.compacted,
        "非 watermark 替换不得让快照失效；小结果增长未超过 limit"
    );
    assert_eq!(model.request_count(), 1);
}

/// `InterruptedAfterCommit` 的 Full 已提交，必须像正常路径一样记录驻留快照。
#[tokio::test]
async fn resident_interrupted_after_commit_records_pressure_snapshot() {
    let bound = TestSession::open().await;
    // 用可控替身在 compaction 落盘后、返回前取消，稳定命中
    // `did_apply && is_cancelled` 的 InterruptedAfterCommit 分支。
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new()
        .with_persistence(store.clone(), thread_id)
        .with_ancestor(vec![make_human(&"i".repeat(RESIDUAL_LIMIT as usize * 3))]);
    transcript.append(make_human("own start"));
    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );
    store.on_compaction_applied({
        let token = context.session.turn.cancel_token.as_ref().clone();
        move || token.cancel()
    });

    let output = run_stage(&context).await.unwrap();
    assert!(
        output.compacted,
        "已提交的 Full 即使随后取消也应报告 compacted"
    );
    assert_eq!(model.request_count(), 1);
    let snapshot = context
        .compact
        .budget_recovery
        .lock()
        .resident_after_full()
        .expect("InterruptedAfterCommit 的 Full 必须记录快照");
    assert!(
        snapshot.total_reserved_bytes() > RESIDUAL_LIMIT,
        "夹具的继承区使 Full 后残余仍超限"
    );
    assert!(!session.transcript().read().entries().is_empty());
}
