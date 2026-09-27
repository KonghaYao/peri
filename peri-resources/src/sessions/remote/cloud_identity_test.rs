//! 显式云端操作身份回归（默认 `#[ignore]`）：**同值往返与同边界复用必须真的生效**。
//!
//! 抓的是内容派生操作 id 的真实缺陷：id 由「store + 标签 + 内容摘要」决定时，第三次
//! 同内容调用会撞上前一次的 id，被当成历史重放**静默丢掉效果**。三条断言覆盖三个面：
//!
//! | 断言 | 抓的是什么 |
//! | --- | --- |
//! | 状态 A→B→A→B 落到 B | 第三次同内容更新必须生效，不是重放 |
//! | 标题 x→y→x→y 落到 y | 同上，命中定向 metadata 更新路径 |
//! | 同边界 rewind 后再追加再 rewind 只留一条 | 同边界复用必须真的裁剪，不是重放 |
//! | 每次调用都有自己的账本行 | 「一次领域调用 = 一次远端操作」在账本上可数 |
//! | `recover_persistence` 返回 `Recovered` | 已结清的 root 必须给出确定结论 |
//!
//! 安全与清理：只操作本轮 run 命名空间（操作 id 以 run 前缀的会话开头，故清理按 run
//! 前缀即命中本轮全部账本行）；结束用正常 mutation 路径删除本轮行并复核计数为 0；
//! 只输出计数/布尔；本机执行面库写在系统临时目录，不进仓库、不含真实历史或凭证。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_identity_
//! ```

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{RewindBoundary, SessionMetaPatch};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::AgentStatus;
use turso_serverless::Value;

use super::cloud_tests::{
    check, classify_engine, failure, http_base_url, numeric_version, scheme_class, session_input,
    synth_binding, unique_run_label, with_cleanup, CloudTarget,
};
use super::connection::{connect_sdk, RemoteTransport, SdkTransport};
use super::mutation::StoreAccess;
use super::sql::{text_at, StatementSpec};
use crate::sessions::data::SessionDataPort;

/// 本会话在远端账本上的行数（操作 id 以会话 id 开头，清理与核对共用这一约定）。
const COUNT_THREAD_LEDGER_SQL: &str =
    "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id LIKE ?1";

/// 实验：同值往返与同边界复用在真引擎上的可观察结果。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_operation_identity_survives_repeat_values() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-id");
    let mut out = target.out();
    out.push("experiment=operation_identity".to_owned());
    out.push(format!("run_prefix_len={}", run.len()));
    out.flush();
    let result = with_cleanup(&target, &run, || async {
        identity_flow(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("operation_identity=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn identity_flow(target: &CloudTarget, run: &str) -> Result<(), String> {
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let root = format!("{run}-id");
    let root_id = peri_acp_types::thread::ThreadId::from(root.as_str());
    let created_at = chrono::Utc::now().to_rfc3339();
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let binding = synth_binding();

    // 标题先给一个显式值：后面用「x → y → x → y」把同内容复用走到头。
    let mut new_session = session_input(&root, &created_at, &binding, &frozen, None);
    new_session.meta.title = Some("x".to_owned());
    writer
        .save_new_session(&new_session)
        .await
        .map_err(failure)?;

    // 状态 A(创建默认 Active) → Done → Active → Done：第三次与第一次**内容完全相同**。
    for status in [AgentStatus::Done, AgentStatus::Active, AgentStatus::Done] {
        writer
            .update_meta(
                &root_id,
                &SessionMetaPatch {
                    title: None,
                    status: Some(status),
                    cancel_policy: None,
                    config: None,
                },
            )
            .await
            .map_err(failure)?;
    }
    // 标题 x → y → x → y：同样第 3 次与第 1 次内容相同。
    for title in ["y", "x", "y"] {
        writer
            .update_meta(
                &root_id,
                &SessionMetaPatch {
                    title: Some(Some(title.to_owned())),
                    status: None,
                    cancel_policy: None,
                    config: None,
                },
            )
            .await
            .map_err(failure)?;
    }

    // 同边界复用：rewind 到 m1 之后追加 m3，再 rewind 到**同一边界**必须真的裁掉 m3。
    let first = PersistedPayload::Message(BaseMessage::human("identity first"));
    let second = PersistedPayload::Message(BaseMessage::ai("identity second"));
    writer
        .append_history(&root_id, &[first.clone(), second.clone()])
        .await
        .map_err(failure)?;
    let boundary = RewindBoundary::KeepThrough(first.id());
    writer
        .rewind_history(&root_id, boundary)
        .await
        .map_err(failure)?;
    let third = PersistedPayload::Message(BaseMessage::human("identity third"));
    writer
        .append_history(&root_id, std::slice::from_ref(&third))
        .await
        .map_err(failure)?;
    writer
        .rewind_history(&root_id, boundary)
        .await
        .map_err(failure)?;

    // 断言走新连接（只读打开），不读本连接的缓存。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;
    let meta = reader.load_meta(&root_id).await.map_err(failure)?;
    check(
        meta.title.as_deref() == Some("y"),
        "title must be the value of the last call, not a replayed earlier one",
    )?;
    let claim = reader
        .load_child_resume_record(&root_id)
        .await
        .map_err(failure)?;
    check(
        claim.status == AgentStatus::Done,
        "status must be the value of the last call, not a replayed earlier one",
    )?;
    let history = reader
        .load_session_history(&root_id)
        .await
        .map_err(failure)?;
    check(
        history.len() == 1 && history[0].id() == first.id(),
        "rewind to the same boundary must trim the appended entry, not be dropped as a replay",
    )?;

    // 全部结清、可恢复。
    let recovery = writer
        .recover_persistence(&root_id)
        .await
        .map_err(failure)?;
    check(
        matches!(
            recovery,
            peri_acp_types::session_resources::PersistenceRecovery::Recovered
        ),
        "a settled root must report Recovered",
    )?;

    // 账本行数：创建 1 + 状态 3 + 标题 3 + 追加 2 + rewind 2 = 11。
    let expected_operations = 11i64;
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let batches = store
        .read_batch(vec![StatementSpec::new(
            COUNT_THREAD_LEDGER_SQL,
            vec![Value::Text(format!("{root}.%"))],
        )])
        .await
        .map_err(failure)?;
    let counted = batches
        .into_iter()
        .next()
        .and_then(|mut rows| rows.pop())
        .and_then(|mut row| row.pop())
        .and_then(|value| match value {
            Value::Integer(number) => Some(number),
            _ => None,
        })
        .unwrap_or(-1);
    store.close().await.map_err(failure)?;
    check(
        counted == expected_operations,
        "each domain call must leave exactly one ledger row of its own",
    )?;

    let mut out = target.out();
    out.push(format!("operations={counted}"));
    out.push("identity=ok".to_owned());
    out.flush();
    writer.close().await.map_err(failure)?;
    reader.close().await.map_err(failure)?;
    Ok(())
}

// ─── 远端形状只读盘点 ─────────────────────────────────────────────────────────

const LIST_TABLES_SQL: &str =
    "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name";
const COUNT_TABLE_SQL: &str =
    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1";
const SELECT_STORE_META_SQL: &str =
    "SELECT schema_version, store_id, contract FROM peri_store_meta WHERE singleton = 0";

/// 只读盘点目标库的连通性、身份标记与表集合：**不建表、不写行、不清理任何对象**。
///
/// 两个用途：统一 schema 之前确认目标库的身份与现有形状；统一之后同一份输出就是
/// 「远端表集合 = 本地 canonical 形状」的真云基线（多出的表会在这里显形）。
#[tokio::test]
#[ignore = "显式 cloud 只读盘点：需要已授权测试库的 .env，默认不跑"]
async fn cloud_store_shape_snapshot() {
    let target = CloudTarget::load();
    match shape_snapshot(&target).await {
        Ok(lines) => {
            let mut out = target.out();
            for line in lines {
                out.push(line);
            }
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn shape_snapshot(target: &CloudTarget) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    lines.push(format!("engine={}", target.endpoint().engine().as_str()));
    lines.push(format!("host_class={}", target.endpoint().host_class()));
    lines.push(format!(
        "scheme_class={}",
        scheme_class(target.endpoint().sdk_url())
    ));

    // `GET /version`（libSQL/sqld 的版本身份入口）：只报状态码与分类，不回显响应正文。
    let base = http_base_url(target.endpoint().sdk_url())
        .map_err(|class| format!("locator cannot rewrite to an http base: {class}"))?;
    let response = reqwest::Client::new()
        .get(format!("{}/version", base.as_str().trim_end_matches('/')))
        .bearer_auth(target.credential().expose())
        .send()
        .await
        .map_err(|error| {
            format!(
                "GET /version transport: {}",
                super::cloud_tests::transport_class(&error)
            )
        })?;
    lines.push(format!("get_version_status={}", response.status().as_u16()));
    let body = response
        .text()
        .await
        .map_err(|_| "GET /version body unreadable".to_owned())?;
    lines.push(format!("get_version_body_class={}", classify_engine(&body)));
    lines.push(format!("get_version_numeric={}", numeric_version(&body)));

    let transport = SdkTransport::new(
        connect_sdk(target.endpoint(), target.credential())
            .await
            .map_err(failure)?,
    );

    for table in crate::sessions::canonical::CANONICAL_TABLES {
        let sql: &'static str = match *table {
            "threads" => "SELECT COUNT(*) FROM threads",
            "messages" => "SELECT COUNT(*) FROM messages",
            "session_bindings" => "SELECT COUNT(*) FROM session_bindings",
            "projects" => "SELECT COUNT(*) FROM projects",
            _ => "SELECT COUNT(*) FROM workspaces",
        };
        let rows = read_rows(&transport, &StatementSpec::bare(sql)).await?;
        let count = rows
            .first()
            .and_then(|row| match row.first() {
                Some(Value::Integer(number)) => Some(*number),
                _ => None,
            })
            .unwrap_or(-1);
        lines.push(format!("rows_{table}={count}"));
    }

    // 服务端的父行检查是跨连接共享的可变状态：读数会决定带 `REFERENCES` 的写入会不会撞约束。
    let foreign_keys = read_rows(&transport, &StatementSpec::bare("PRAGMA foreign_keys")).await?;
    let foreign_keys = foreign_keys
        .first()
        .and_then(|row| row.first())
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "<unreadable>".to_owned());
    lines.push(format!("pragma_foreign_keys={foreign_keys}"));

    let table_rows = read_rows(&transport, &StatementSpec::bare(LIST_TABLES_SQL)).await?;
    let tables: Vec<String> = table_rows
        .iter()
        .filter_map(|row| text_at(row, 0).map(str::to_owned))
        .collect();
    lines.push(format!("table_count={}", tables.len()));
    lines.push(format!("tables={}", tables.join(",")));

    let meta_probe = read_rows(
        &transport,
        &StatementSpec::new(
            COUNT_TABLE_SQL,
            vec![Value::Text("peri_store_meta".to_owned())],
        ),
    )
    .await?;
    let meta_present = meta_probe
        .first()
        .and_then(|row| match row.first() {
            Some(Value::Integer(count)) => Some(*count > 0),
            _ => None,
        })
        .ok_or_else(|| "meta presence probe returned an unexpected shape".to_owned())?;
    lines.push(format!("peri_store_meta_present={meta_present}"));
    if meta_present {
        let meta = read_rows(&transport, &StatementSpec::bare(SELECT_STORE_META_SQL)).await?;
        match meta.first() {
            Some(row) => {
                let version = row.first().and_then(|value| match value {
                    Value::Integer(number) => Some(*number),
                    _ => None,
                });
                lines.push(format!("peri_store_meta_schema_version={version:?}"));
                let contract = text_at(row, 2).unwrap_or("<non-text>");
                lines.push(format!("peri_store_meta_contract={contract}"));
                let store_prefix = text_at(row, 1)
                    .map(|id| id.chars().take(8).collect::<String>())
                    .unwrap_or_else(|| "<non-text>".to_owned());
                lines.push(format!("peri_store_meta_store_id_prefix={store_prefix}"));
            }
            // 表在但没有单行：同样是「身份不可解释」，如实报出来，不当成空库。
            None => lines.push("peri_store_meta_rows=0".to_owned()),
        }
    }

    transport
        .close()
        .await
        .map_err(|error| format!("close: {:?}", super::failure::classify(&error)))?;
    Ok(lines)
}

/// 一条只读语句：失败只报脱敏分类，不回显语句或响应内容。
async fn read_rows(
    transport: &SdkTransport,
    spec: &StatementSpec,
) -> Result<Vec<Vec<Value>>, String> {
    transport
        .sql_values(spec)
        .await
        .map_err(|error| format!("{:?}", super::failure::classify(&error)))
}
