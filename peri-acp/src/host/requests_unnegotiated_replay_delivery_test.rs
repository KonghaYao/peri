use super::*;

/// 未协商连接 + 含 Full 归档（excluded 行）的会话：回放**包含归档原文**，且逐条内容与顺序
/// 等于 `load_session_history` 的 canonical 序列。
#[tokio::test]
async fn streamed_canonical_replay_delivers_the_archived_bodies_in_canonical_order() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-archive").await;
    fixture
        .append(&[
            BaseMessage::human("归档：第一轮问题"),
            BaseMessage::ai("归档：第一轮回答"),
            BaseMessage::human("有效：第二轮问题"),
            BaseMessage::ai("有效：第二轮回答"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    fixture.archive(&payload_ids(&canonical)[..2]).await;
    fixture.set_caps(PeriCaps::default());

    let effective = fixture.effective().await;
    assert_eq!(
        effective.len(),
        2,
        "夹具前提：归档后有效集合只剩两条（回放不得以它为来源）"
    );
    fixture.reset_reads();

    fixture.load().await.unwrap();

    let expected: Vec<String> = canonical.iter().map(payload_text).collect();
    assert_eq!(expected.len(), 4);
    assert!(
        expected.contains(&"归档：第一轮问题".to_owned()),
        "夹具前提：canonical 读取含归档原文"
    );
    assert_eq!(
        fixture.texts(),
        expected,
        "回放必须逐条交付 canonical 完整历史（含归档原文），顺序一致"
    );
    assert_eq!(fixture.spy.segment_calls().len(), 1, "四条一段读完");
    assert_eq!(fixture.spy.full_reads(), 0, "生产后端路径不读全量");
}

/// wire 级：标准客户端（什么都没声明）的初始 load 回放通知里**一个 Peri 私有字段都没有**，
/// 且全量通知与响应里不出现任何分页私有字段名。
///
/// 判据是**前缀 / 字段名**而不是枚举：回放通知上不设任何枚举白名单，只要出现 `peri*` 就失败。
/// （响应里既有的非分页扩展，例如 `_meta.peri.sessionWorkspaceV1`，属于别的切片，不在本判据内。）
#[tokio::test]
async fn streamed_canonical_replay_wire_carries_no_peri_private_fields_for_a_standard_client() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-wire").await;
    fixture
        .append(&[BaseMessage::human("首屏问题"), BaseMessage::ai("首屏回答")])
        .await;
    fixture.set_caps(PeriCaps::default());

    let response = fixture.load().await.unwrap();

    let keys = replay_keys(&fixture);
    assert!(!keys.is_empty(), "回放通知上确实有内容可查");
    let peri: Vec<&String> = keys.iter().filter(|key| key.starts_with("peri")).collect();
    assert!(
        peri.is_empty(),
        "未协商连接的回放通知不得出现 Peri 私有字段: {peri:?}"
    );
    assert_no_pagination_fields(&wire_keys(&fixture, &response));
    assert!(
        !wire_keys(&fixture, &response)
            .iter()
            .any(|key| key == "periLoadGeneration"),
        "未声明 peri.replay 的连接不会拿到代次"
    );
    assert_eq!(fixture.texts(), vec!["首屏问题", "首屏回答"]);
}

/// `periReplay` 的门是 `caps.replay`（回放扩展）：声明后回放通知带标记，但不再携带
/// 已删除的 `periLoadGeneration`，也不带任何分页私有字段。
///
/// `caps.replay` 取自**当前连接**：本用例显式声明连接 caps，同时刻意把会话级
/// 注册表写成 `PeriCaps::default()`——门若回读注册表，这里就会一条标记都收不到。
#[tokio::test]
async fn streamed_canonical_replay_carries_only_the_replay_marker() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture
        .create_session("streamed-canonical-replay-marker")
        .await;
    fixture
        .append(&[BaseMessage::human("标记问题"), BaseMessage::ai("标记回答")])
        .await;
    fixture.set_connection_caps(PeriCaps {
        replay: true,
        ..PeriCaps::default()
    });
    fixture.set_caps(PeriCaps::default());

    let response = fixture.load().await.unwrap();

    assert!(
        response["_meta"].get("periLoadGeneration").is_none(),
        "旧回放代次不再写入响应: {response}"
    );
    let updates = fixture.updates();
    assert_eq!(updates.len(), 2);
    for params in &updates {
        let meta = &params["update"]["_meta"];
        assert_eq!(meta["periReplay"], Value::Bool(true));
        assert!(
            meta.get("periLoadGeneration").is_none(),
            "旧回放代次不再写入通知: {meta}"
        );
        assert!(
            meta.get("periEntryId").is_none() && meta.get("periPosition").is_none(),
            "标准回放不得到条目身份 / 位置戳: {meta}"
        );
    }
    assert_no_pagination_fields(&wire_keys(&fixture, &response));
}

/// 边界：空会话（没有任何自有行）回放成功且不产出一条通知。
#[tokio::test]
async fn streamed_canonical_replay_of_an_empty_session_succeeds_without_notifications() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-empty").await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load().await.unwrap();

    assert!(fixture.texts().is_empty());
    assert_eq!(
        fixture.spy.segment_calls().len(),
        1,
        "空会话也走同一条首段读取，之后凭 next_cursor = None 结束"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
}

/// 边界：只有继承区的会话（首段一次性给出继承区 payload）照常交付，顺序不变。
///
/// 脚本只替代「存储侧把继承区装配进首段」这一步（S9a 的段契约，由 peri-resources 的段用例
/// 覆盖）；本用例验证的是 ACP 出口对首段继承区 payload 的投影与顺序。
#[tokio::test]
async fn streamed_canonical_replay_delivers_an_inherited_only_segment() {
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("继承区问题")),
        PersistedPayload::Message(BaseMessage::ai("继承区回答")),
    ];
    let mut fixture = ReplayFixture::new(SpyMode::ScriptedFirstSegment(inherited.clone())).await;
    fixture.create_session("streamed-canonical-inherited").await;
    fixture.set_caps(PeriCaps::default());
    assert!(
        fixture.canonical().await.is_empty(),
        "夹具前提：本线程没有任何自有行"
    );
    fixture.reset_reads();

    fixture.load().await.unwrap();

    assert_eq!(fixture.texts(), vec!["继承区问题", "继承区回答"]);
    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].payloads, inherited.len());
    assert_eq!(fixture.spy.full_reads(), 0);
}

/// 边界：单条正文超预算时独占一段，仍被**完整**交付（完整回放不允许静默丢内容）。
#[tokio::test]
async fn streamed_canonical_replay_delivers_an_oversized_body_whole() {
    let huge = "x".repeat(530_000);
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-oversized").await;
    fixture
        .append(&[BaseMessage::human("短行"), BaseMessage::ai(huge.clone())])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load().await.unwrap();

    let texts = fixture.texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0], "短行");
    assert_eq!(texts[1].len(), huge.len(), "超大正文必须整条交付");
    assert_eq!(texts[1], huge);

    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 2, "超预算的那条独占一段: {calls:?}");
    assert_eq!(calls[0].payloads, 1, "第一段只收下能进预算的那条");
    assert_eq!(calls[1].payloads, 1, "超大正文独占第二段");
}

/// `AlreadyLive`（会话已在内存）分支：未协商连接的 canonical 回放**同样从存储流式读取**，
/// 不得回退成只重放热态 `history_payloads`。夹具把热态有效集合缩到只剩尾条目：若回放源是
/// 内存，归档原文就会消失。
#[tokio::test]
async fn streamed_canonical_replay_of_a_live_session_still_reads_storage() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    let sid = register_session_with_history(
        &mut fixture.sessions,
        fixture.cwd.to_str().unwrap(),
        &fixture.cfg,
    )
    .await;
    fixture.id = sid.clone();
    fixture.set_caps(PeriCaps::default());
    let live_ids = payload_ids(&fixture.canonical().await);
    fixture.archive(&live_ids[..2]).await;
    let live_tail = fixture.sessions[&sid]
        .history_payloads
        .last()
        .unwrap()
        .clone();
    {
        let state = fixture.sessions.get_mut(&sid).unwrap();
        state.history_payloads = vec![live_tail];
    }
    fixture.reset_reads();

    fixture.load().await.unwrap();

    assert_eq!(
        fixture.texts(),
        vec!["第一轮用户问题", "第一轮回答", "第二轮用户问题"],
        "AlreadyLive 也必须交付存储里的 canonical 历史（含归档原文）"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
}
