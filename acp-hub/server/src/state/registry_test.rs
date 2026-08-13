//! Registry 写者测试：instance 视图 upsert 接线（§7.1/§12.4）——Applier
//! 字段语义（8 字段 + status 序列化）与写链（RegistryState → DocManager
//! 全局 registry 写者 → Registry Doc 广播）。

use std::sync::Arc;

use yrs::{Map, ReadTxn, Transact};

use acp_hub_proto::schema::{InstanceStatus, InstanceView};

use crate::control::StoreSink;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocCommand, DocManager, DocUpdate};
use crate::state::factory::{Factory, ROOT};
use crate::state::registry::{RegistryApplier, RegistryError, RegistryState};

/// 真实 RegistryState（经 DocManager 全局 registry 写者，§5.2 单写）：
/// session_registry_test 同构装配，写回依赖存活写者与 Registry Doc。
async fn test_registry() -> (RegistryState, Arc<DocManager>) {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(DocManager::new(BatchConfig::default(), sink.clone()));
    let registry = doc.registry();
    (registry, doc)
}

fn view(id: &str, heartbeat: &str, chat_count: u32) -> InstanceView {
    InstanceView {
        id: id.to_string(),
        hostname: "host1".to_string(),
        status: InstanceStatus::Online,
        token_id: "tok-1".to_string(),
        registered_at: "2026-08-01T00:00:00Z".to_string(),
        last_heartbeat: heartbeat.to_string(),
        chat_count,
    }
}

/// 轮询广播队列直至收到 REGISTRY update（写者是异步 task，广播发生在
/// 提交期间）。注意：广播为 yrs **增量**，无快照基线不可独立解码内容，
/// 故仅断言写链广播存在（内容语义由 Applier 单测与 gateway e2e 快照
/// 重放覆盖）。
async fn wait_registry_update(rx: &mut tokio::sync::mpsc::UnboundedReceiver<DocUpdate>) {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        while let Ok(u) = rx.try_recv() {
            if u.doc == acp_hub_proto::conn::DocId::REGISTRY {
                return;
            }
        }
    }
    panic!("REGISTRY update 应到达（写链未生效）");
}

/// Applier 单测：upsert 写 8 字段（含 `instance_status_str` 序列化
/// "online"，§5.5 机器视图）。
#[test]
fn applier_writes_instance_fields() {
    let mut applier = RegistryApplier::new(Factory::new().create_registry_doc());
    applier
        .apply(&DocCommand::RegistryUpsertInstance(view(
            "m1",
            "2026-08-07T00:00:01Z",
            2,
        )))
        .unwrap();
    let txn = applier.doc.transact();
    let root = txn.get_map(ROOT).unwrap();
    let instances = root
        .get(&txn, "instances")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(instances.len(&txn), 1);
    let mm = instances
        .get(&txn, "m1")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(mm.get(&txn, "id"), Some(yrs::Out::Any("m1".into())));
    assert_eq!(
        mm.get(&txn, "hostname"),
        Some(yrs::Out::Any("host1".into()))
    );
    assert_eq!(mm.get(&txn, "status"), Some(yrs::Out::Any("online".into())));
    assert_eq!(
        mm.get(&txn, "token_id"),
        Some(yrs::Out::Any("tok-1".into()))
    );
    assert_eq!(
        mm.get(&txn, "registered_at"),
        Some(yrs::Out::Any("2026-08-01T00:00:00Z".into()))
    );
    assert_eq!(
        mm.get(&txn, "last_heartbeat"),
        Some(yrs::Out::Any("2026-08-07T00:00:01Z".into()))
    );
    assert_eq!(mm.get(&txn, "chat_count"), Some(yrs::Out::Any(2f64.into())));
}

/// RegistryState 端到端（§5.2 单写链）：upsert_instance → 全局 registry
/// 写者 → REGISTRY update 广播；同 id 二次 upsert 字段覆盖（gateway 侧
/// registered_at 由 hello 局部变量复用首值，§3.2 方案 A；Applier 层为
/// 纯覆盖语义）。
#[tokio::test]
async fn upsert_instance_writes_and_overwrites_registry_doc() {
    let (reg, doc) = test_registry().await;
    let mut rx = doc.subscribe_updates().await;
    reg.upsert_instance(view("m1", "2026-08-07T00:00:01Z", 1))
        .await
        .unwrap();
    // 心跳覆盖：last_heartbeat 刷新 + chat_count 更新。
    reg.upsert_instance(view("m1", "2026-08-07T00:00:02Z", 3))
        .await
        .unwrap();
    // 写链生效：两条命令均经全局写者落 Registry Doc 并广播（增量）。
    wait_registry_update(&mut rx).await;
    wait_registry_update(&mut rx).await;
}

/// set_instance_status：未注册 instance → NotFound（registry.rs:283-288）。
#[tokio::test]
async fn set_instance_status_unknown_instance_not_found() {
    let (reg, _doc) = test_registry().await;
    assert!(matches!(
        reg.set_instance_status("ghost", InstanceStatus::Offline)
            .await,
        Err(RegistryError::NotFound(_))
    ));
}

/// 重启语义（§5.5）：已存在条目（历史会话）不被 upsert 复活——status 由
/// set_chat_status 权威管理；title/instance_id 保持。这是「客户端订阅
/// 触发的 open_chat 把启动对账标记的 ended 覆盖回 accepting」的回归
/// 测试（实测 836c8a3e：面板重连 → 订阅 chat:{sid} → open_chat →
/// upsert 全字段覆盖）。
#[test]
fn upsert_chat_does_not_resurrect_existing_entry() {
    use crate::state::doc_manager::DocCommand;
    use acp_hub_proto::schema::ChatSummary;

    let mut applier = RegistryApplier::new(yrs::Doc::new());
    // 首次 upsert（create 语义）：新条目全量写入（accepting）。
    let mut summary = ChatSummary {
        id: "s-1".into(),
        instance_id: "m1".into(),
        title: "历史标题".into(),
        status: "accepting".into(),
        gap: None,
        updated_at: "2026-08-01T00:00:00Z".into(),
        cwd: "/".into(),
        workspace_id: None,
    };
    applier
        .apply(&DocCommand::RegistryUpsertChat(summary.clone()))
        .unwrap();
    // 启动对账：标记 ended（终态，视图保留）。
    applier.set_chat_status("s-1", "ended").unwrap();
    // 客户端订阅 open_chat 的 upsert：重启后内存表为空 → 空
    // instance_id/title + accepting，updated_at 刷新。
    summary.instance_id.clear();
    summary.title.clear();
    summary.status = "accepting".into();
    summary.updated_at = "2026-08-02T00:00:00Z".into();
    applier
        .apply(&DocCommand::RegistryUpsertChat(summary))
        .unwrap();

    let txn = applier.doc.transact();
    let root = txn.get_map(ROOT).unwrap();
    let sessions = root
        .get(&txn, "chats")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let sm = sessions
        .get(&txn, "s-1")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        sm.get(&txn, "status"),
        Some(yrs::Out::Any("ended".into())),
        "已 ended 会话不得被 upsert 复活"
    );
    assert_eq!(
        sm.get(&txn, "title"),
        Some(yrs::Out::Any("历史标题".into()))
    );
    assert_eq!(
        sm.get(&txn, "instance_id"),
        Some(yrs::Out::Any("m1".into()))
    );
    assert_eq!(
        sm.get(&txn, "updated_at"),
        Some(yrs::Out::Any("2026-08-02T00:00:00Z".into())),
        "已存在条目仍刷新 updated_at（列表排序）"
    );
}
