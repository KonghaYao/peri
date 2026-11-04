use super::*;

/// 有界证据：段读取被多次调用，**没有任何一次返回全量**，且未协商路径不调用全量读取。
#[tokio::test]
async fn streamed_canonical_replay_reads_bounded_segments_and_never_the_full_history() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-bounded").await;
    let messages: Vec<BaseMessage> = (0..REPLAY_SEGMENT_ROWS + 10)
        .map(|index| {
            if index % 2 == 0 {
                BaseMessage::human(format!("第 {index} 条"))
            } else {
                BaseMessage::ai(format!("第 {index} 条回答"))
            }
        })
        .collect();
    fixture.append(&messages).await;
    fixture.set_caps(PeriCaps::default());
    let total = fixture.canonical().await.len();
    assert_eq!(total, messages.len());
    fixture.reset_reads();

    fixture.load().await.unwrap();

    let calls = fixture.spy.segment_calls();
    assert!(
        calls.len() >= 2,
        "越过一段规模的历史必须分多段读取，实际 {} 次",
        calls.len()
    );
    assert!(
        calls.iter().all(|call| call.payloads < total),
        "没有任何一次读取允许返回全量（total = {total}）: {calls:?}"
    );
    assert!(
        calls.iter().all(|call| call.limit == REPLAY_SEGMENT_ROWS
            && call.byte_budget == REPLAY_SEGMENT_BYTE_BUDGET),
        "段规模必须是命名的回放常量: {calls:?}"
    );
    assert!(
        calls[0].cursor.is_none() && calls[1..].iter().all(|call| call.cursor.is_some()),
        "首段无游标，后续段必须带游标: {calls:?}"
    );
    assert_eq!(fixture.spy.full_reads(), 0, "流式路径不读全量");
    assert_eq!(fixture.texts().len(), total, "逐条交付，无重复无遗漏");
}

/// 失败关闭：段读取损坏 → 本次 load 以错误结束，且**不产生半份首屏 + 成功响应**。
#[tokio::test]
async fn streamed_canonical_replay_failure_closes_the_load() {
    let mut fixture = ReplayFixture::new(SpyMode::Corrupt).await;
    fixture.create_session("streamed-canonical-corrupt").await;
    fixture
        .append(&[BaseMessage::human("不会交付"), BaseMessage::ai("不会交付")])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load()
        .await
        .expect_err("段读取失败必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");
    assert!(
        fixture.updates().is_empty(),
        "首段就失败：一条通知都不该送出"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
    assert_eq!(
        fixture.canonical().await.len(),
        2,
        "失败的交付不改动存储事实"
    );
}

/// 锚点消失（回放途中的并发精确删除）同样失败关闭：不允许把空段当成「回放结束」。
#[tokio::test]
async fn streamed_canonical_replay_fails_closed_when_the_anchor_vanishes_mid_stream() {
    let mut fixture = ReplayFixture::new(SpyMode::AnchorVanishes).await;
    fixture.create_session("streamed-canonical-anchor").await;
    let messages: Vec<BaseMessage> = (0..REPLAY_SEGMENT_ROWS + 5)
        .map(|index| BaseMessage::human(format!("第 {index} 条")))
        .collect();
    fixture.append(&messages).await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load()
        .await
        .expect_err("锚点消失必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");

    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 2, "首段成功、续读失败: {calls:?}");
    assert!(calls[0].error.is_none());
    assert!(
        calls[1].error.is_some(),
        "第二次读取必须带着真实存储的失败回来: {calls:?}"
    );
    assert!(
        !fixture.texts().is_empty() && fixture.texts().len() < messages.len(),
        "失败发生在半途（这正是「不得留下半份首屏 + 成功响应」的场景）"
    );
}

/// 回退：后端没有向前段读取时**显式**回退到全量 canonical 读取，回放仍然完整。
///
/// 判据是**调用记录**（首段读过一次 + 全量读取恰好一次）与交付内容，不是日志文本：生产代码
/// 那条 warn 是诊断，把它断言下来要依赖 tracing 的**进程级** callsite 兴趣缓存与线程局部
/// 订阅（`set_default` + `rebuild_interest_cache`），默认并发下会被同进程其它用例的
/// `rebuild_interest_cache` / 订阅装卸搅掉（`.recon/S9-boundedness.md` 第 6 节 R8：同一条
/// 命令首跑失败、复跑通过，产品断言两次都通过）。回退本身的行为由下面三条断言钉住。
#[tokio::test]
async fn streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported(
) {
    let mut fixture = ReplayFixture::new(SpyMode::Unsupported).await;
    fixture.create_session("streamed-canonical-fallback").await;
    fixture
        .append(&[
            BaseMessage::human("回退：归档问题"),
            BaseMessage::ai("回退：归档回答"),
            BaseMessage::human("回退：有效问题"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    fixture.archive(&payload_ids(&canonical)[..2]).await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load().await.unwrap();

    let expected: Vec<String> = canonical.iter().map(payload_text).collect();
    assert_eq!(fixture.texts(), expected, "回退同样交付完整 canonical 历史");
    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 1, "回退只发生在首段之前");
    assert!(
        calls[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("behavior is unsupported here")),
        "首段那句的失败理由是「后端没有这条读取」（回退的触发条件）: {calls:?}"
    );
    assert_eq!(fixture.spy.full_reads(), 1, "回退经全量读取一次");
}
