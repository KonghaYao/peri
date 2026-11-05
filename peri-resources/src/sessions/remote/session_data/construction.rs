//! 远端会话数据的打开与构造：读回 store 身份、按访问意图补齐 schema，装配 adapter。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::session_resources::SessionResourceResult;
use tokio::sync::RwLock;
use turso_serverless::Value;

use super::super::credentials::SessionStoreCredential;
use super::super::endpoint::RemoteEndpoint;
use super::super::generation::{ConnectionFactory, ConnectionGate, RemoteConnectionFactory};
use super::super::mutation::StoreAccess;
use super::super::schema::{StoreIdentityRead, StoreShapeRead};
use super::super::sql::StatementSpec;
use super::super::{schema_upgrade, session_schema};
use super::connection_lifecycle::ConnectionSlot;
use super::session_open::{refuse_legacy_shape, resolve_open_machine_id};
use super::{
    open_step, open_verdict, probe_shape, verify_legacy_open_identity, OpenStep, RemoteSessionData,
    StoreInitialization,
};

impl RemoteSessionData {
    /// 打开远程会话数据：读回 store 身份；未初始化时只在可写打开下创建本任务 schema。
    ///
    /// 返回值里的 [`StoreInitialization`] 是「首次登记」的唯一证据，且只有一个来源：
    /// [`open_verdict`] 对身份竞争结论（[`StoreIdentityOutcome`]）的映射——只有本事务确切
    /// 插入元数据行并提交（`Created`）才是 `CreatedByThisOpen`。初始化结果未知（丢响应、
    /// 超时）会让本次打开直接失败：既不发身份，也不发创建事实。
    ///
    /// 四条拒绝路径都不猜：版本/契约不认识、元数据形状不可解释、会话表形状与契约声明的
    /// 代数不符（[`probe_shape`]，只读）、只读打开遇上尚未初始化的 store（没有 schema 就
    /// 没有会话事实可读，也不越权建表）。
    pub(in crate::sessions::remote) async fn open(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
        access: StoreAccess,
    ) -> SessionResourceResult<(Self, StoreInitialization)> {
        Self::open_with_machine(endpoint, credential, access, None).await
    }

    pub(in crate::sessions::remote) async fn open_with_machine(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
        access: StoreAccess,
        supplied_machine_id: Option<String>,
    ) -> SessionResourceResult<(Self, StoreInitialization)> {
        let gate = Arc::new(ConnectionGate::default());
        let factory: Arc<dyn ConnectionFactory> = Arc::new(RemoteConnectionFactory::new(
            endpoint.clone(),
            credential.duplicate(),
            access,
            Arc::clone(&gate),
        ));
        let store = factory.connect().await?;
        // 打开路径的两份只读事实：身份（版本 + 契约 + store id）与会话表形状。
        // 后者连同必要索引在一次只读批中探测；列形状不符即拒绝。
        let read = store.read_identity().await?;
        let shape = probe_shape(&store, &read).await?;
        let schema_work = shape.clone();
        let legacy_shape = matches!(&read, StoreIdentityRead::Present(snapshot) if !snapshot.matches_build())
            && access == StoreAccess::ReadOnly;
        let legacy_open_identity = legacy_shape.then(|| read.clone());
        let step = open_step(read, shape, access)?;
        let upgraded = matches!(step, OpenStep::Upgrade(_));
        let (store_id, initialization) = match step {
            OpenStep::Existing(store_id) => (store_id, StoreInitialization::Existing),
            OpenStep::Upgrade(snapshot) => {
                schema_upgrade::upgrade(&store, &snapshot).await?;
                (snapshot.store_id, StoreInitialization::Existing)
            }
            // 空库：写打开在这里才参与身份竞争，结论由本事务的结果给出（见 [`open_verdict`]），
            // 不由「刚才读到空库」推定创建。
            OpenStep::NeedsInitialization => {
                // 空库与「旧形状的库」在身份读取上都读作未初始化：先问一次后者，命中即拒绝。
                refuse_legacy_shape(&store).await?;
                open_verdict(store.initialize_store().await?)
            }
        };
        let machine_id = resolve_open_machine_id(supplied_machine_id, access).await?;
        // 健康的当前库已通过严格列形状与必要索引检查，无须每次重放 DDL。
        // 身份早于会话表建立的库仍需补齐；缺索引时只补索引。
        if access == StoreAccess::ReadWrite {
            // 父行检查先归位：canonical 形状里的外键在远端没有可满足的父行（见方法文档）。
            store.force_parent_checks_off().await?;
            if !upgraded {
                let plan = match schema_work {
                    Some(StoreShapeRead::NeedsCurrentRepair {
                        missing_indexes,
                        missing_close_intents,
                    }) => session_schema::repair_plan(missing_indexes, missing_close_intents),
                    Some(StoreShapeRead::Unbuilt) | None => session_schema::initialization_plan(),
                    Some(StoreShapeRead::Consistent | StoreShapeRead::Drifted(_)) => Vec::new(),
                };
                if !plan.is_empty() {
                    store.apply_schema(plan).await?;
                }
            }
            let machine_exists = store
                .fetch_row(&StatementSpec::new(
                    "SELECT id FROM machines WHERE id = ?1",
                    vec![Value::Text(machine_id.clone())],
                ))
                .await?
                .is_some();
            if !machine_exists {
                store
                .apply_schema(vec![StatementSpec::new(
                    "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')",
                    vec![Value::Text(machine_id.clone())],
                )])
                .await?;
            }
        }
        // 旧形状只读打开可能与另一实例的原子升级竞争；不可带过期的读法返回。
        if let Some(initial) = legacy_open_identity {
            verify_legacy_open_identity(&initial, &store.read_identity().await?)?;
        }
        Ok((
            Self {
                machine_id,
                slot: RwLock::new(ConnectionSlot::serving(store)),
                factory,
                gate,
                store_id,
                legacy_shape,
                roots: RwLock::new(HashMap::new()),
            },
            initialization,
        ))
    }
}
