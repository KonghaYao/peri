//! 能力门控与私有字段（REQ §5.3、验收 A26 的 wire 面）。
//!
//! 从 `history_page_wire_test.rs` 按职责拆出（STD-SIZE-001：单文件 ≤1000 行），语义未改动：
//! 用例体、断言与 helper 逐字节搬移。夹具与跨文件共享 helper 在父模块里，经 `use super::*` 取用。

use super::*;

// ── 独立复核（S9-recheck：未协商连接的完整回放与私有语义边界）──────────────────

/// JSON 树里出现过的全部键（递归，含 `_meta` 内层）。
fn s9_recheck_all_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.push(key.clone());
                s9_recheck_all_keys(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| s9_recheck_all_keys(item, out)),
        _ => {}
    }
}

/// 跑一次未协商分页的**真实 ServerLoop** `session/load`：返回（回放文本序列，通知 + 响应
/// 里出现过的全部键，响应体）。
///
/// 会话级 caps 注册表的值由调用方决定（模拟「这条会话此前被谁写过」）；连接级事实一律是
/// 标准客户端（`initialize` 声明里没有任何 peri 扩展）。
async fn s9_recheck_unnegotiated_load(
    fixture: &PageFixture,
    session_registry_caps: PeriCaps,
) -> (Vec<String>, Vec<String>, Value) {
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), session_registry_caps);

    let canonical_len = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .len();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        Arc::clone(&fixture.sessions),
        Some(PeriCaps::default()),
    )
    .await;
    let sid = fixture.sid.clone();

    let for_load = Arc::clone(&client);
    let load = tokio::spawn(async move {
        for_load
            .send_request("session/load", json!({ "sessionId": sid }))
            .await
    });

    let mut texts: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while texts.len() < canonical_len {
        let incoming = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("回放必须在时限内发完")
            .expect("客户端通道在响应之前不得关闭");
        let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        else {
            continue;
        };
        s9_recheck_all_keys(&params, &mut keys);
        if method != "session/update" {
            continue;
        }
        if let (Some(kind), Some(text)) = (
            params["update"]["sessionUpdate"].as_str(),
            params["update"]["content"]["text"].as_str(),
        ) {
            if matches!(kind, "user_message_chunk" | "agent_message_chunk") {
                texts.push(text.to_owned());
            }
        }
    }
    let reply = load
        .await
        .expect("load 任务不得 panic")
        .expect("标准客户端的 session/load 必须成功");
    s9_recheck_all_keys(&reply, &mut keys);
    task.abort();
    (texts, keys, reply)
}

/// 未协商分页的连接（标准 ACP 客户端）经真实 `ServerLoop` 的 `session/load`：
/// 必须交付完整 canonical 历史（含 Full 归档原文），且响应与通知里不得出现分页私有字段。
#[tokio::test]
#[serial]
async fn s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(2).await;
    assert_eq!(archived.len(), 2, "夹具前提：最旧两条 own 条目被 Full 归档");

    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    assert!(
        canonical.iter().any(|text| text == "第一轮用户问题"),
        "夹具前提：canonical 读取含归档原文: {canonical:?}"
    );

    let (texts, keys, reply) = s9_recheck_unnegotiated_load(&fixture, PeriCaps::default()).await;

    assert_eq!(
        texts, canonical,
        "未协商连接必须收到完整 canonical 回放（含归档原文），顺序一致"
    );
    assert!(
        reply.get("nextCursor").is_none() && reply.get("snapshotToken").is_none(),
        "未协商连接的响应不得携带分页字段: {reply}"
    );
    for forbidden in ["nextCursor", "snapshotToken", "periEntryId", "periPosition"] {
        assert!(
            !keys.iter().any(|key| key == forbidden),
            "分页私有字段 {forbidden} 不得出现在未协商连接的 wire 上"
        );
    }
}

/// REQ §5.3 的反例回归：**会话级 caps 注册表**已被先前的 peri 连接写过（`replay` 与
/// `history_pagination` 都是 true）时，当前这个未协商任何 peri 扩展的标准连接**不得**收到任何
/// Peri 私有字段——能力以**当前连接**的协商结果为准，不继承会话内保存的旧值（验收 A25）。
///
/// 本用例原为 S9 独立复核留下的 `#[ignore]` 反例探针（`.recon/S9-verification.md` 第 8 节：
/// 当时 `periReplay` 4 条通知 + `periLoadGeneration` 响应与通知都泄漏）；S9c 把回放标记的门
/// 改成连接事实后转为回归用例。
#[tokio::test]
#[serial]
async fn s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;

    let (_texts, keys, _reply) = s9_recheck_unnegotiated_load(&fixture, pagination_caps()).await;

    let leaked: Vec<String> = [
        "nextCursor",
        "snapshotToken",
        "periEntryId",
        "periPosition",
        "periLoadGeneration",
        "periReplay",
    ]
    .into_iter()
    .filter_map(|forbidden| {
        let count = keys.iter().filter(|key| *key == forbidden).count();
        (count > 0).then(|| format!("{forbidden} x{count}"))
    })
    .collect();
    assert!(
        leaked.is_empty(),
        "未协商分页的连接不得继承会话级 caps 带来的私有字段；实际命中: {leaked:?}"
    );
}

/// 跑一次**协商过 peri 能力**的**真实 ServerLoop** `session/load`：返回（回放通知上出现过的
/// 代次，通知 + 响应里出现过的全部键，响应体）。
///
/// 与 [`s9_recheck_unnegotiated_load`] 只差连接声明。通知读取用 `biased` 的 `select!`：只要
/// 通道里还有通知就先取通知，取完才观察 load 完成——回放通知总在响应之前送出，因此不会漏。
async fn s9_recheck_negotiated_load(
    fixture: &PageFixture,
    session_registry_caps: PeriCaps,
    connection_caps: PeriCaps,
) -> (Vec<String>, Vec<String>, Value) {
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), session_registry_caps);

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        Arc::clone(&fixture.sessions),
        Some(connection_caps),
    )
    .await;
    let sid = fixture.sid.clone();

    let for_load = Arc::clone(&client);
    let mut load = Box::pin(tokio::spawn(async move {
        for_load
            .send_request("session/load", json!({ "sessionId": sid }))
            .await
    }));

    let mut keys: Vec<String> = Vec::new();
    let mut generations: Vec<String> = Vec::new();
    let reply = loop {
        tokio::select! {
            biased;
            incoming = client.recv() => {
                match incoming {
                    Some(crate::transport::types::IncomingMessage::Notification { params, .. }) => {
                        s9_recheck_all_keys(&params, &mut keys);
                        if let Some(generation) =
                            params["update"]["_meta"]["periLoadGeneration"].as_str()
                        {
                            generations.push(generation.to_owned());
                        }
                    }
                    // 非通知消息（响应不经这条通道）与通道关闭都只说明「回放通知不会再有」：
                    // 等 load 自身的结果，不做任何 busy loop。
                    Some(_) => {}
                    None => break (&mut load).await.expect("load 任务不得 panic"),
                }
            }
            done = &mut load => break done.expect("load 任务不得 panic"),
        }
    };
    let reply = reply.expect("协商连接的 session/load 必须成功");
    s9_recheck_all_keys(&reply, &mut keys);
    task.abort();
    (generations, keys, reply)
}

/// 门控没被改死：同一场景下**当前连接**协商了 peri 能力的客户端**仍然**收到 `periReplay` 与
/// `periLoadGeneration`（响应与回放通知同一个代次）。
///
/// 会话级注册表刻意写 `PeriCaps::default()`：门若回读注册表（无论继承旧值还是被旧值压住），
/// 本用例的标记断言都会失败。
#[tokio::test]
#[serial]
async fn s9_recheck_a_negotiated_connection_still_receives_the_replay_marks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;

    let (generations, keys, reply) =
        s9_recheck_negotiated_load(&fixture, PeriCaps::default(), pagination_caps()).await;

    assert!(
        keys.iter().any(|key| key == "periReplay"),
        "协商过回放能力的连接仍必须收到 periReplay: {keys:?}"
    );
    assert!(
        keys.iter().any(|key| key == "periLoadGeneration"),
        "协商过回放能力的连接仍必须收到 periLoadGeneration: {keys:?}"
    );
    assert!(
        !generations.is_empty(),
        "回放通知上必须带代次（不只是响应）: {keys:?}"
    );
    assert!(
        generations
            .iter()
            .all(|seen| Some(seen.as_str()) == reply["_meta"]["periLoadGeneration"].as_str()),
        "回放通知与成功响应必须带同一个代次（REQ-H04）：{generations:?} 对 {reply}"
    );
}

// ── 独立复核（S9c-verification：`initialize` **请求**到连接事实的接线）────────────
//
// 上段三条 `s9_recheck_*` 都把连接 caps 直接 `commit_initialize` 进 `ConnectionContext`，绕过
// `initialize` 请求本身；以下三条走**真实请求**（`initialize` → `handle_initialize` →
// `commit_initialize`）驱动同一份连接事实，覆盖 §5.3 三类连接（标准客户端 / peri 客户端 /
// 从未 `initialize` 的进程内路径）。夹具、`ServerLoop`、SQLite 门面与上段同源。

/// 六个 Peri 私有回放 / 分页字段（复核判据的完整口径）。
const S9C_PRIVATE_FIELDS: [&str; 6] = [
    "periReplay",
    "periLoadGeneration",
    "nextCursor",
    "snapshotToken",
    "periEntryId",
    "periPosition",
];

/// 标准 ACP 客户端的 `initialize` 声明：带客户端能力字段，但**没有任何** peri 扩展。
fn s9c_standard_initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": false } },
    })
}

/// peri 客户端的 `initialize` 声明：回放标记 + 历史分页（TUI / SDK 的形状）。
fn s9c_peri_initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "_meta": { "peri.replay": true, "peri.historyPagination": true },
        },
    })
}

/// 一次 load 的 wire 事实：回放正文序列、出现过的全部键、逐条通知的 `_meta`。
async fn s9c_wire_facts(
    client: &crate::transport::mpsc::MpscClientTransport,
    reply: &Value,
) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let notifications = s9c_drain_notifications(client).await;
    assert!(
        !notifications.is_empty(),
        "load 必须发出通知（否则本用例是空转）"
    );
    let mut keys: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut metas: Vec<Value> = Vec::new();
    for (method, params) in &notifications {
        s9_recheck_all_keys(params, &mut keys);
        if method != "session/update" {
            continue;
        }
        let update = &params["update"];
        if let (Some(kind), Some(text)) = (
            update["sessionUpdate"].as_str(),
            update["content"]["text"].as_str(),
        ) {
            if matches!(kind, "user_message_chunk" | "agent_message_chunk") {
                texts.push(text.to_owned());
            }
        }
        if let Some(meta) = update.get("_meta") {
            metas.push(meta.clone());
        }
    }
    s9_recheck_all_keys(reply, &mut keys);
    (texts, keys, metas)
}

/// 命中的私有字段（`字段 x次数`），空 = 一个都没出现。
fn s9c_hits(keys: &[String]) -> Vec<String> {
    S9C_PRIVATE_FIELDS
        .into_iter()
        .filter_map(|field| {
            let count = keys.iter().filter(|key| *key == field).count();
            (count > 0).then(|| format!("{field} x{count}"))
        })
        .collect()
}

/// 用例 A（复核者新增）：连接**经真实 `initialize` 请求**声明为标准客户端（声明里没有 peri
/// 扩展），会话级注册表已被先前的 peri 连接写满（`replay` + `historyPagination`）。
///
/// 断言：响应与全部通知**一个私有字段都没有**，且交付正文仍是完整 canonical（含 Full 归档
/// 原文）。与上段 `s9_recheck_unnegotiated_load` 的差别是**接线面**：那三条把 caps 直接写进
/// `ConnectionContext`，本用例驱动 `initialize` 请求 → `initialize_peri_caps` →
/// `commit_initialize` 这条真实链路；接线若断（解析位置错、未落连接），只有本用例会红。
#[tokio::test]
#[serial]
async fn s9c_verification_an_initialize_without_peri_extensions_yields_no_private_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    assert!(
        canonical.iter().any(|text| text == "第一轮用户问题"),
        "夹具前提：canonical 含被 Full 归档的原文: {canonical:?}"
    );

    // 这条会话此前被 peri 连接注册过：注册表里存着旧客户端的全能力值。
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), pagination_caps());

    let cfg = Arc::clone(&fixture.cfg);
    // 连接侧**不预置**任何协商值：门必须由下面这条真实 `initialize` 请求建立。
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    client
        .send_request("initialize", s9c_standard_initialize_params())
        .await
        .expect("标准客户端的 initialize 必须成功");
    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("未协商 peri 扩展的连接，session/load 必须成功");
    let (texts, keys, _metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    assert_eq!(
        s9c_hits(&keys),
        Vec::<String>::new(),
        "initialize 未声明 peri 扩展的连接不得继承会话级注册表的旧能力: {keys:?}"
    );
    assert_eq!(
        texts, canonical,
        "未协商连接必须收到完整 canonical 回放（含归档原文）"
    );
}

/// 用例 B（复核者新增）：连接**经真实 `initialize` 请求**声明了 `peri.replay` +
/// `peri.historyPagination`，而会话级注册表**刻意**留着 `PeriCaps::default()`。
///
/// 断言：回放标记与分页字段**照旧出现**（门没被改成「永远关闭」），且通知与响应同一个代次。
#[tokio::test]
#[serial]
async fn s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), PeriCaps::default());

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    client
        .send_request("initialize", s9c_peri_initialize_params())
        .await
        .expect("peri 客户端的 initialize 必须成功");
    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("协商过 peri 能力的连接，session/load 必须成功");
    let (texts, keys, metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    assert!(
        texts.len() >= 3,
        "协商连接回放的是热态有效集合，正文非空: {texts:?}"
    );
    assert!(
        reply["nextCursor"].is_string() && reply["snapshotToken"].is_string(),
        "协商过分页的连接，响应必须带首游标与快照身份: {reply}"
    );
    let generation = reply["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("协商过回放能力的连接，响应必须带代次");
    assert!(
        !metas.is_empty(),
        "回放通知必须带 meta（否则本用例是空转）: {keys:?}"
    );
    for meta in &metas {
        assert_eq!(
            meta["periReplay"],
            Value::Bool(true),
            "协商过回放能力的连接，每条回放通知都必须带 periReplay: {meta}"
        );
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.to_owned()),
            "通知与响应必须同源代次（REQ-H04）: {meta}"
        );
        assert!(
            meta["periEntryId"].is_string(),
            "协商过分页的连接，条目身份必须照旧打上: {meta}"
        );
    }
}

/// 用例 C（复核者新增）：连接**从未 `initialize`**（进程内 TUI / MPSC 路径），会话级注册表
/// 是 `PeriCaps::default()`——即历史语义完全靠连接兜底决定。
///
/// 断言：回放标记照旧（历史全能力语义不缩水），且分页准入仍失败关闭（一个分页字段都没有）。
#[tokio::test]
#[serial]
async fn s9c_verification_a_never_initialized_in_process_connection_keeps_the_replay_marks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    // 注册表刻意留空能力：本用例的标记若出现，只可能来自连接的兜底全能力。
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), PeriCaps::default());

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("进程内路径的 session/load 必须成功");
    let (texts, keys, metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    let generation = reply["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("未 initialize 的进程内路径保持历史全能力语义：响应仍带代次")
        .to_owned();
    assert!(
        metas
            .iter()
            .any(|meta| meta["periReplay"] == Value::Bool(true)),
        "未 initialize 的进程内路径仍必须收到 periReplay: {metas:?}"
    );
    for meta in &metas {
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.clone()),
            "通知与响应必须同源代次: {meta}"
        );
    }
    for forbidden in ["nextCursor", "snapshotToken", "periEntryId", "periPosition"] {
        assert!(
            !keys.iter().any(|key| key == forbidden),
            "未协商分页的连接不得出现分页字段 {forbidden}: {keys:?}"
        );
    }
    assert_eq!(
        texts, canonical,
        "未协商分页的进程内路径仍走完整 canonical 回放"
    );
}
