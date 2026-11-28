//! 远端会话数据的打开与构造：读回 store 身份、按访问意图补齐 schema，装配 adapter。

use std::collections::HashMap;
use std::sync::Arc;

use super::super::sql::Value;
use peri_acp_types::session_resources::SessionResourceResult;
use tokio::sync::RwLock;

use super::super::credentials::SessionStoreCredential;
use super::super::endpoint::RemoteEndpoint;
use super::super::generation::{ConnectionFactory, ConnectionGate, RemoteConnectionFactory};
use super::super::mutation::StoreAccess;
use super::super::pool_policy::SessionStorePoolPolicy;
use super::super::sql::StatementSpec;
use super::connection_lifecycle::ConnectionSlot;

use super::RemoteSessionData;
use super::StoreInitialization;

impl RemoteSessionData {
    /// 打开远程会话数据：读回 store 身份；未初始化时只在可写打开下创建本任务 schema。
    ///
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
        Self::open_with_machine_and_policy(
            endpoint,
            credential,
            access,
            supplied_machine_id,
            SessionStorePoolPolicy::default(),
        )
        .await
    }

    /// Same open, with the deployment's pool capacity.
    pub(in crate::sessions::remote) async fn open_with_machine_and_policy(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
        access: StoreAccess,
        supplied_machine_id: Option<String>,
        pool_policy: SessionStorePoolPolicy,
    ) -> SessionResourceResult<(Self, StoreInitialization)> {
        let gate = Arc::new(ConnectionGate::default());
        let factory: Arc<dyn ConnectionFactory> = Arc::new(RemoteConnectionFactory::new(
            endpoint.clone(),
            credential.duplicate(),
            access,
            Arc::clone(&gate),
            pool_policy,
        ));
        let store = factory.connect().await?;
        let (store_id, initialization) = super::session_open::open_schema(&store, access).await?;
        let machine_id =
            super::session_open::resolve_open_machine_id(supplied_machine_id, access).await?;
        if access == StoreAccess::ReadWrite {
            store
                .apply_schema(vec![StatementSpec::new(
                "INSERT INTO machines (id, name, identity_kind) VALUES ($1, '我的电脑', 'known')
                 ON CONFLICT (id) DO NOTHING",
                vec![Value::Text(machine_id.clone())],
            )])
                .await?;
        }
        Ok((
            Self {
                machine_id,
                slot: RwLock::new(ConnectionSlot::serving(store)),
                factory,
                gate,
                store_id,
                roots: RwLock::new(HashMap::new()),
            },
            initialization,
        ))
    }
}
