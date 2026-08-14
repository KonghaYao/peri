//! 控制面装配（架构 §8.4.1/§8.6/§17.2，设计稿 `f5-channel-control.md` §14）。
//!
//! [`Hub`] 是**唯一装配点**：Store 恢复 → StoreSink（UpdateSink 薄 adapter +
//! 快照镜像）→ DocManager → 注册表/协调器/广播器 → Gateway；后台周期任务
//! （instance 离线 sweep + nonce sweep 合并单一 tick，设计稿决策 5）。
//!
//! [`StoreSink`]：F5 提供的 `UpdateSink` 生产实现（设计稿 §14 决策 5——F6
//! 未提供时本 feature 装配薄 adapter）：
//!
//! - 落盘：chat/control update → `ChatStore::append_update`（水位同步推进）；
//!   registry update → `<data_dir>/registry.snapshot` + bounded
//!   `<data_dir>/registry.log`（blob 格式，复用 persist
//!   `read_blob`/`write_blob`）；
//! - **快照镜像**：启动时从 `Store` 恢复产物（`ChatReplay`：快照 + 日志
//!   记录）重建 yrs 镜像，运行期 `persist_update` 时同步应用——gateway 快照
//!   与 broadcaster 增量的**单一真相**（F4 DocManager 无启动重放 API，镜像
//!   承担视图恢复；TUI 重连快照 = 完整历史，P1/P3）；
//! - **广播**：镜像更新流（`subscribe()`）供 Broadcaster attach——快照与增量
//!   同源同 clientID，客户端应用无 CRDT 分叉。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Seek;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use acp_hub_proto::conn::DocId;
use yrs::types::AsPrelim;
use yrs::updates::decoder::Decode as _;
use yrs::{Map, ReadTxn, Transact};

use crate::channel::Broadcaster;
use crate::channel::ChannelDeps;
use crate::channel::CommandCoordinator;
use crate::channel::ConnectionRegistry;
use crate::channel::Gateway;
use crate::channel::RelayEventHandler;
use crate::config::Config;
use crate::control::ChatRegistry;
use crate::control::InstanceRegistry;
use crate::control::ProjectService;
use crate::persist::metadata::MetadataStore;
use crate::persist::update_log::{read_blob, sync_dir, write_blob};
use crate::persist::Store;
use crate::state::doc_manager::{BatchConfig, DocManager, DocUpdate, PersistError, UpdateSink};
use crate::state::factory::{DocKind, Factory};
use crate::state::registry::{DegradeCause, RegistryState};
use crate::state::view_store::encode_state_as_update;
use acp_hub_proto::version::{
    CHAT_DOC_SCHEMA_VERSION, REGISTRY_DOC_SCHEMA_VERSION, SESSION_DOC_SCHEMA_VERSION,
};

/// registry 更新日志文件名（`<data_dir>/registry.log`，blob 格式；§8.4 同
/// fsync 纪律）。
pub const REGISTRY_LOG_FILE: &str = "registry.log";
pub const REGISTRY_SNAPSHOT_FILE: &str = "registry.snapshot";
const REGISTRY_SNAPSHOT_TMP_FILE: &str = "registry.snapshot.tmp";
const REGISTRY_LEGACY_BACKUP_FILE: &str = "registry.log.legacy-v1";
const REGISTRY_COMPACT_BYTES: u64 = 8 * 1024 * 1024;

/// hub 装配/运行错误。
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// Store 打开失败。
    #[error("store open failed: {0}")]
    Store(String),
    /// 监听绑定失败。
    #[error("bind failed: {0}")]
    Bind(std::io::Error),
    /// 周期任务退出。
    #[error("maintenance task failed")]
    Maintenance,
}

/// 控制面装配：全部组件实例化与接线（§8.6）。
pub struct Hub {
    /// 持久化 Store。
    pub store: Arc<Store>,
    /// UpdateSink 薄 adapter + 快照镜像。
    pub sink: Arc<StoreSink>,
    /// DocManager（唯一提交边界）。
    pub doc: Arc<DocManager>,
    /// 命令协调器。
    pub coordinator: Arc<CommandCoordinator>,
    /// instance 入站消费。
    pub relay: Arc<RelayEventHandler>,
    /// instance 注册表。
    pub instance: Arc<InstanceRegistry>,
    /// chat 注册表。
    pub chats: Arc<ChatRegistry>,
    /// 连接注册表。
    pub conns: Arc<ConnectionRegistry>,
    /// 广播器。
    pub broadcast: Arc<Broadcaster>,
    /// gateway。
    pub gateway: Gateway,
    /// auth 服务（周期 nonce sweep 共享）。
    pub auth: Arc<tokio::sync::Mutex<crate::auth::AuthService>>,
    /// Registry 状态源（§17.2 degraded 判定 / §8.4.1 恢复门禁）。
    pub registry: RegistryState,
    pub metadata: Arc<MetadataStore>,
    pub projects: ProjectService,
}

impl Hub {
    /// 装配（main `run_with` 调用；store 须已完成 `recover`，§8.4.1）。
    #[allow(clippy::too_many_arguments)]
    pub async fn assemble(
        cfg: &Config,
        store: Arc<Store>,
        auth: Arc<tokio::sync::Mutex<crate::auth::AuthService>>,
    ) -> Result<Hub, HubError> {
        // 1. UpdateSink 薄 adapter（落盘 + 镜像 + 广播流）。
        let sink = Arc::new(StoreSink::new(store.clone()).await?);
        // 2. DocManager（BatchConfig 从 §16 默认映射）。
        let batch = BatchConfig {
            batch_window: cfg.microbatch_window,
            batch_bytes: 4096,
            chat_queue: cfg.command_queue_cap,
        };
        let doc = Arc::new(DocManager::with_recovered_registry(
            batch,
            sink.clone(),
            sink.recovered_registry_doc(),
        ));
        let registry = doc.registry();
        let metadata = Arc::new(
            MetadataStore::open(store.data_dir())
                .await
                .map_err(|e| HubError::Store(e.to_string()))?,
        );
        // 3. 注册表/协调器/广播器。
        let chats = Arc::new(ChatRegistry::new(registry.clone()));
        let instance = Arc::new(InstanceRegistry::new(
            cfg.offline_timeout,
            cfg.spawn_timeout,
            chats.as_ref().clone(),
        ));
        let relay = Arc::new(RelayEventHandler::new(
            doc.clone(),
            chats.as_ref().clone(),
            instance.clone(),
            registry.clone(),
        ));
        let coordinator = Arc::new(CommandCoordinator::new(
            store.clone(),
            doc.clone(),
            instance.clone(),
            chats.as_ref().clone(),
            relay.clone(),
            &batch,
            cfg.acp_cmd.clone(),
            cfg.spawn_timeout,
            cfg.initialize_timeout,
            cfg.binding_timeout,
        ));
        let projects = ProjectService::new(metadata.clone(), registry.clone());
        coordinator.install_project_service(projects.clone()).await;
        coordinator.install_history_sink(sink.clone()).await;
        coordinator
            .reconcile_prompt_delivery_after_restart()
            .await
            .map_err(HubError::Store)?;
        // §4.4：create 全局去重索引（跨 server 重启有效）——store 已完成
        // recover（main 前置），从 outbox 重建后才接受连接。
        coordinator.rebuild_create_index().await;
        // §6.3 workspace 扩展：工作区内存注册表从 Registry Doc 重建（跨
        // 重启后 create 携带 workspace_id 仍能解析 cwd）。
        coordinator.rebuild_workspaces().await;
        projects
            .import_legacy_workspaces()
            .await
            .map_err(|e| HubError::Store(e.to_string()))?;
        projects
            .import_legacy_sessions()
            .await
            .map_err(|e| HubError::Store(e.to_string()))?;
        metadata
            .recover_after_restart()
            .await
            .map_err(|e| HubError::Store(e.to_string()))?;
        projects
            .reproject()
            .await
            .map_err(|e| HubError::Store(e.to_string()))?;
        // §6.3：session/list 轮询（10s 全量同步投影；server 侧，见
        // CommandCoordinator::spawn_session_poller 决策注释）。
        coordinator.spawn_session_poller();
        let broadcast = Arc::new(Broadcaster::new(
            cfg.backpressure_soft_bytes,
            cfg.backpressure_hard_bytes,
        ));
        // 广播流 = StoreSink 镜像增量（单一真相，见模块文档）。
        broadcast.attach(sink.subscribe().await);
        let conns = Arc::new(ConnectionRegistry::new(cfg.connection_quota));
        let deps = ChannelDeps {
            coordinator: coordinator.clone(),
            broadcast: broadcast.clone(),
            instance: instance.clone(),
            chats: chats.clone(),
            conns: conns.clone(),
        };
        let gateway = Gateway::new(
            Arc::new(cfg.clone()),
            auth.clone(),
            conns.clone(),
            deps,
            relay.clone(),
            doc.clone(),
            sink.clone(),
            registry.clone(),
        );
        // §8.4.1 不变量 4：恢复期门禁——instance 重连（hello）对账完成前
        // 不开门（gateway 在首次 hello 后 clear_restarting；Restarting 期间
        // 拒绝新 committed 承诺）。
        if let Err(e) = registry.set_restarting().await {
            warn!(error = ?e, "set_restarting failed (registry write)");
        }
        // 启动对账（§5.5 重启语义）：registry.log 重放的 chat 是历史 chat，其
        // ACP 进程在重启时必然全部终止——非终态统一标记 ended（Ended =
        // "ACP 进程退出（终态，视图保留）"），面板显示"已结束"而非误导为
        // 可对话；终态（ended/closed/crashed）保持不变。清单在
        // 清单由 StoreSink 的唯一恢复 pass 缓存；不再为装配阶段重复扫描日志。
        {
            let stale = sink.recovered_stale_chats().to_vec();
            let mut marked = 0usize;
            for chat_id in stale {
                match registry.set_chat_status(&chat_id, "ended").await {
                    Ok(()) => marked += 1,
                    Err(e) => warn!(chat_id = %chat_id, error = ?e, "startup reconcile failed"),
                }
            }
            if marked > 0 {
                info!(
                    count = marked,
                    "startup reconcile: stale chats marked ended"
                );
            }
        }
        Ok(Hub {
            store,
            sink,
            doc,
            coordinator,
            relay,
            instance,
            chats,
            conns,
            broadcast,
            gateway,
            auth,
            registry,
            metadata,
            projects,
        })
    }

    /// 启动对账辅助（§5.5 重启语义）：枚举 registry doc 中**非终态** chat。
    /// 重启后这些 chat 的 ACP 进程必然已终止（instance 启动清理 + kill_on_drop），
    /// 由 assemble 统一标记 ended（终态 ended/closed/crashed 保持不变）。
    fn enum_stale_registry_chats(doc: &yrs::Doc) -> Vec<String> {
        const TERMINAL: [&str; 3] = ["ended", "closed", "crashed"];
        let txn = doc.transact();
        let Some(root) = txn.get_map("root") else {
            return Vec::new();
        };
        let Some(chats) = root
            .get(&txn, "chats")
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (chat_id, v) in chats.iter(&txn) {
            let status = v
                .cast::<yrs::MapRef>()
                .ok()
                .and_then(|m| m.get(&txn, "status"))
                .and_then(|s| s.cast::<String>().ok())
                .unwrap_or_default();
            if !TERMINAL.contains(&status.as_str()) {
                out.push(chat_id.to_string());
            }
        }
        out
    }

    /// 运行入口（main `run_with` 调用）：绑定监听 → 周期任务 + gateway 并发
    /// 运行 → 优雅关闭（§8.6 顺序：停止接收新 Action → 完成在途提交 →
    /// 释放引用 → 关闭连接）。
    ///
    /// `signal` 为 SIGINT/SIGTERM 通知（main 装配）。
    pub async fn run_server(
        self,
        cfg: &Config,
        signal: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<()> {
        let addr = std::net::SocketAddr::new(cfg.listen_addr, cfg.listen_port);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(HubError::Bind)?;
        info!(%addr, "acp-hub-server listening");

        // 周期任务：instance 离线 sweep + nonce sweep（单一 tick，设计稿
        // 决策 5；判定粒度 1s【决策】）。
        let instance = self.instance.clone();
        let relay = self.relay.clone();
        let auth = self.auth.clone();
        let maintenance = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let now = std::time::Instant::now();
                for instance_id in instance.sweep_offline(now).await {
                    // §7.1 离线即刻生效（心跳超时路径）。
                    if let Err(e) = relay.on_instance_disconnect(&instance_id).await {
                        warn!(instance_id, error = ?e, "offline cleanup failed");
                    }
                }
                // nonce sweep（§9.2：30s 窗口过期清理）。
                auth.lock().await.nonces_mut().sweep(now);
            }
        });

        // 优雅关闭：停止 accept + 周期任务 → 连接自然关闭。
        let gateway = self.gateway.clone();
        tokio::select! {
            _ = gateway.run(listener) => {
                warn!("gateway run exited unexpectedly");
            }
            _ = signal => {
                info!("shutdown signal received; closing connections");
            }
        }
        maintenance.abort();
        info!("acp-hub-server stopped");
        Ok(())
    }

    /// Degraded 判定入口（§17.2）：非 Healthy → 拒绝新 committed 承诺（新
    /// Action 返回 retryable 错误，§8.4 落盘失败语义同源）。
    pub fn can_accept_committed(&self) -> bool {
        self.gateway.can_accept_committed()
    }

    /// 恢复不变量失败上报（§8.4.1 不变量 5）：main 在 `Store::recover()`
    /// 返回 `degraded` 时调用 → Degraded（拒绝新 committed 承诺，§17.2）。
    pub async fn report_restore_degraded(&self) {
        if let Err(e) = self
            .registry
            .report_condition(DegradeCause::RestoreInvariant)
            .await
        {
            warn!(error = ?e, "restore invariant degraded report failed");
        }
    }
}

/// UpdateSink 生产实现（F5 薄 adapter）：落盘 + 快照镜像 + 广播流。
///
/// 并发模型：chat/control 镜像按 DocId 索引（`RwLock<HashMap>`，每次
/// `persist_update` 写锁；镜像 doc 为 yrs 句柄，`apply_update_v1` 需要
/// 独占事务——DocManager writer 是每 chat 单写者，天然串行）。
pub struct StoreSink {
    store: Arc<Store>,
    factory: Factory,
    /// 镜像 doc（chat:{cid} / control:{cid} / hub:registry）。
    docs: RwLock<HashMap<DocId, yrs::Doc>>,
    /// 镜像更新广播（broadcaster attach 消费）。
    broadcast: RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>,
    /// 落盘水位推进（session_id → (epoch, seq)；epoch 从 Store 水位继承，
    /// seq 每次 persist_update +1【决策：DocManager 不传帧 seq，adapter 以
    /// 提交粒度推进——水位只作补推起点近似，聚合器幂等兜底】）。
    seq: RwLock<HashMap<String, (u32, u64)>>,
    /// registry 独立日志文件。
    registry_log: PathBuf,
    registry_snapshot: PathBuf,
    registry_snapshot_tmp: PathBuf,
    registry_legacy_backup: PathBuf,
    registry_io: tokio::sync::Mutex<()>,
    /// 启动时已经合并好的单条全量 state。DocManager 与 stale-chat 对账
    /// 各自应用这一条，而不是反复扫描整个历史日志。
    recovered_registry_update: Option<Vec<u8>>,
    recovered_stale_chats: Vec<String>,
}

impl StoreSink {
    /// 打开并重建镜像（须在 `Store::recover` 后调用）。
    pub async fn new(store: Arc<Store>) -> Result<Self, HubError> {
        let factory = Factory::new();
        let data_dir = store.data_dir().to_path_buf();
        let registry_log = data_dir.join(REGISTRY_LOG_FILE);
        let registry_snapshot = data_dir.join(REGISTRY_SNAPSHOT_FILE);
        let registry_snapshot_tmp = data_dir.join(REGISTRY_SNAPSHOT_TMP_FILE);
        let registry_legacy_backup = data_dir.join(REGISTRY_LEGACY_BACKUP_FILE);
        if registry_snapshot_tmp.exists() {
            std::fs::remove_file(&registry_snapshot_tmp)
                .map_err(|e| HubError::Store(format!("remove stale registry snapshot tmp: {e}")))?;
        }
        let recovered = recover_registry_doc(&factory, &registry_snapshot, &registry_log)?;
        let recovered_registry_update = recovered.doc.as_ref().map(encode_state_as_update);
        let recovered_stale_chats = recovered
            .doc
            .as_ref()
            .map(Hub::enum_stale_registry_chats)
            .unwrap_or_default();
        let mut initial_docs = HashMap::new();
        if let Some(doc) = recovered.doc {
            initial_docs.insert(DocId::REGISTRY, doc);
        }
        let sink = StoreSink {
            store,
            factory,
            docs: RwLock::new(initial_docs),
            broadcast: RwLock::new(Vec::new()),
            seq: RwLock::new(HashMap::new()),
            registry_log,
            registry_snapshot,
            registry_snapshot_tmp,
            registry_legacy_backup,
            registry_io: tokio::sync::Mutex::new(()),
            recovered_registry_update,
            recovered_stale_chats,
        };
        sink.rebuild_mirror(recovered.records).await?;
        sink.compact_registry_if_needed().await?;
        Ok(sink)
    }

    /// 恢复后的 Registry Doc（§8.4.1）：registry.log 非空时以新 doc 应用全部
    /// 记录（历史 client 的结构/数据）；为空返回 None（writer 用全新结构
    /// doc）。供 DocManager 装配注入，避免 writer 与历史结构的 CRDT LWW
    /// 冲突（镜像内容不可见）。
    pub fn recovered_registry_doc(&self) -> Option<yrs::Doc> {
        let update = self.recovered_registry_update.as_ref()?;
        let doc = yrs::Doc::new();
        apply_update(&doc, update);
        Some(doc)
    }

    fn recovered_stale_chats(&self) -> &[String] {
        &self.recovered_stale_chats
    }

    /// 启动重建：逐 chat 从 `ChatReplay`（快照 + 日志记录）应用；
    /// registry 从独立日志重放。
    async fn rebuild_mirror(&self, registry_records: usize) -> Result<(), HubError> {
        let mut docs = self.docs.write().await;
        let mut seq = self.seq.write().await;
        // 各 session：快照 + 日志记录。
        for (sid, replay) in self.replay_by_chat() {
            let mut chat_updates: Vec<Vec<u8>> = Vec::new();
            let mut control_updates: Vec<Vec<u8>> = Vec::new();
            if let Some(snap) = &replay.snapshot {
                for (doc_id, bytes) in &snap.docs {
                    let target = if doc_id.as_str().starts_with("chat:") {
                        &mut chat_updates
                    } else {
                        &mut control_updates
                    };
                    target.push(bytes.clone());
                }
            }
            for record in &replay.records {
                for (doc_id, bytes) in &record.docs {
                    let target = if doc_id.as_str().starts_with("chat:") {
                        &mut chat_updates
                    } else {
                        &mut control_updates
                    };
                    target.push(bytes.clone());
                }
            }
            let chat = create_mirror_doc(&self.factory, DocKind::Chat, &chat_updates);
            let control = create_mirror_doc(&self.factory, DocKind::Session, &control_updates);
            docs.insert(DocId::chat(&sid), chat);
            docs.insert(DocId::session(&sid), control);
            seq.insert(sid, (replay.watermark.epoch, replay.watermark.last_seq));
        }
        info!(
            chats = docs.len() / 2,
            registry_records, "sink mirror rebuilt"
        );
        Ok(())
    }

    async fn compact_registry_if_needed(&self) -> Result<(), HubError> {
        let bytes = std::fs::metadata(&self.registry_log)
            .map(|m| m.len())
            .unwrap_or(0);
        if bytes <= REGISTRY_COMPACT_BYTES {
            return Ok(());
        }
        let _guard = self.registry_io.lock().await;
        let update = {
            let docs = self.docs.read().await;
            docs.get(&DocId::REGISTRY)
                .map(|doc| materialize_registry_update(&self.factory, doc))
                .transpose()?
        };
        let Some(update) = update else { return Ok(()) };
        compact_registry_log(
            &self.registry_snapshot,
            &self.registry_snapshot_tmp,
            &self.registry_log,
            &self.registry_legacy_backup,
            &update,
        )
        .map_err(|e| HubError::Store(format!("registry compact failed: {e}")))?;
        info!(
            old_bytes = bytes,
            snapshot_bytes = update.len(),
            "registry log compacted"
        );
        Ok(())
    }

    /// Store 中已恢复 chat 的重放产物（按目录名排序保证确定性，§7）。
    fn replay_by_chat(&self) -> Vec<(String, crate::persist::store::ChatReplay)> {
        let mut out: Vec<(String, crate::persist::store::ChatReplay)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.store.data_dir().join("chats")) {
            let mut dirs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect();
            dirs.sort();
            for dir in dirs {
                let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Ok(sid) = uuid::Uuid::parse_str(name) else {
                    continue;
                };
                if let Some(chat) = self.store.chat(sid) {
                    if let Some(replay) = chat.replay_outcome() {
                        out.push((sid.to_string(), replay));
                    }
                }
            }
        }
        out
    }

    /// 全量快照（gateway 订阅推送；§4.6 步骤 3）。
    ///
    /// 返回 `(state_update, projection_version)`；doc 未打开 → None（空会话
    /// 视图由客户端按空 doc 处理）。
    pub async fn snapshot(&self, doc: &DocId) -> Option<(Vec<u8>, u32)> {
        let docs = self.docs.read().await;
        let d = docs.get(doc)?;
        let state = encode_state_as_update(d);
        let version = projection_version(d);
        Some((state, version))
    }

    /// Startup-only cross-store repair for a Hub-owned v2 prompt entry. The
    /// mirror is already reconstructed at this point; the generated delta is
    /// appended through the normal UpdateSink durability path before the
    /// gateway can accept clients.
    pub async fn reconcile_prompt_entry_delivery(
        &self,
        chat_id: &str,
        entry_id: &str,
        delivery_state: &str,
        delivery_error_code: Option<&str>,
    ) -> Result<bool, HubError> {
        let doc_id = DocId::chat(chat_id);
        let update = {
            let mut docs = self.docs.write().await;
            let Some(doc) = docs.get_mut(&doc_id) else {
                return Ok(false);
            };
            let before = doc.transact().state_vector();
            let changed = {
                let mut txn = doc.transact_mut();
                let Some(root) = txn.get_map(crate::state::factory::ROOT) else {
                    return Ok(false);
                };
                crate::state::chat_writer::set_prompt_entry_delivery(
                    &mut txn,
                    &root,
                    entry_id,
                    delivery_state,
                    delivery_error_code,
                    None,
                )
            };
            if !changed {
                return Ok(false);
            }
            let update = doc.transact().encode_state_as_update_v1(&before);
            update
        };
        self.persist_update(doc_id, update)
            .await
            .map_err(|error| HubError::Store(error.to_string()))?;
        Ok(true)
    }

    /// 镜像更新广播订阅（hub 装配时 broadcaster attach）。
    pub async fn subscribe(&self) -> mpsc::UnboundedReceiver<DocUpdate> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.broadcast.write().await.push(tx);
        rx
    }
}

#[async_trait]
impl UpdateSink for StoreSink {
    async fn persist_update(&self, doc: DocId, update: Vec<u8>) -> Result<(), PersistError> {
        // 1. 镜像应用 + 广播（先于落盘失败检查：镜像与日志同源，落盘失败时
        //    degraded 由 Store 置位）。
        {
            let mut docs = self.docs.write().await;
            if docs.get(&doc).is_none() {
                // doc 未打开（新 chat 未重建/Registry 首写）：惰性创建
                // （先应用业务 update、后幂等补结构——见 [`create_mirror_doc`]）。
                let kind = match doc.as_str() {
                    s if s.starts_with("chat:") => Some(DocKind::Chat),
                    s if s.starts_with("session:") => Some(DocKind::Session),
                    "hub:registry" => Some(DocKind::Registry),
                    _ => None,
                };
                match kind {
                    Some(kind) => {
                        let d =
                            create_mirror_doc(&self.factory, kind, std::slice::from_ref(&update));
                        docs.insert(doc.clone(), d);
                    }
                    None => return Err(PersistError(format!("unknown doc: {doc}"))),
                }
            } else if let Some(target) = docs.get_mut(&doc) {
                apply_update(target, &update);
            }
            let broadcast = self.broadcast.read().await.clone();
            for tx in &broadcast {
                let _ = tx.send(DocUpdate {
                    doc: doc.clone(),
                    update: update.clone(),
                });
            }
        }
        // 2. 落盘。
        let result = match doc.as_str() {
            s if s.starts_with("chat:") || s.starts_with("session:") => {
                let Some(sid) = doc.as_str().split_once(':').map(|(_, s)| s) else {
                    return Err(PersistError("malformed doc id".into()));
                };
                let Some(sid_uuid) = uuid::Uuid::parse_str(sid).ok() else {
                    return Err(PersistError("non-uuid control doc".into()));
                };
                let Some(chat) = self.store.chat(sid_uuid) else {
                    return Err(PersistError(format!("chat not found: {sid}")));
                };
                // 水位推进（提交粒度，见结构文档）。
                let (epoch, seq) = {
                    let mut seqs = self.seq.write().await;
                    let current = seqs.get(sid).copied().unwrap_or((0, 0));
                    let next = (current.0, current.1 + 1);
                    seqs.insert(sid.to_string(), next);
                    next
                };
                chat.append_update(epoch, seq, &[(doc.clone(), update.as_slice())])
                    .await
                    .map_err(|e| PersistError(e.to_string()))
            }
            "hub:registry" => {
                // registry 独立日志（blob 格式，§8.4 同 fsync 纪律）。
                let _guard = self.registry_io.lock().await;
                append_registry_record(&self.registry_log, &update)
                    .map_err(|e| PersistError(e.to_string()))?;
                let bytes = std::fs::metadata(&self.registry_log)
                    .map(|m| m.len())
                    .unwrap_or(0);
                if bytes > REGISTRY_COMPACT_BYTES {
                    let snapshot = {
                        let docs = self.docs.read().await;
                        docs.get(&DocId::REGISTRY)
                            .map(|doc| materialize_registry_update(&self.factory, doc))
                            .transpose()
                            .map_err(|e| PersistError(e.to_string()))?
                    };
                    if let Some(snapshot) = snapshot {
                        compact_registry_log(
                            &self.registry_snapshot,
                            &self.registry_snapshot_tmp,
                            &self.registry_log,
                            &self.registry_legacy_backup,
                            &snapshot,
                        )
                        .map_err(|e| PersistError(e.to_string()))?;
                    }
                }
                Ok(())
            }
            other => Err(PersistError(format!("unknown doc: {other}"))),
        };
        if result.is_err() {
            warn!(doc = %doc, "sink persist failed (store degraded)");
        }
        result
    }
}

/// 镜像 doc 创建与结构补齐（§5.6/§8.4.1「Doc 补齐」）。
///
/// **顺序纪律**：先应用业务 update、后幂等补结构。预初始化（`Factory`
/// 建全结构）再应用业务 update 会引入 CRDT LWW 冲突——镜像 doc 的
/// `projection_version=0`/`schema_version` 初始化写入与业务写入不同 client，
/// 同键合并以 client id 排序，**初始化值可能覆盖业务值**（快照
/// `projection_version` 恒 0）。先应用业务 update 后，以 `schema_version`
/// 占位使 [`Factory::ensure_schema`] 走「已有版本」分支（`patch_missing`
/// 只补缺失键、不覆盖已有业务值）。
fn create_mirror_doc(factory: &Factory, kind: DocKind, updates: &[Vec<u8>]) -> yrs::Doc {
    let mut doc = yrs::Doc::new();
    for update in updates {
        apply_update(&doc, update);
    }
    let version = match kind {
        DocKind::Chat => CHAT_DOC_SCHEMA_VERSION,
        DocKind::Session => SESSION_DOC_SCHEMA_VERSION,
        DocKind::Registry => REGISTRY_DOC_SCHEMA_VERSION,
    };
    {
        use yrs::{Transact, WriteTxn};
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(crate::state::factory::ROOT);
        if root.get(&txn, "schema_version").is_none() {
            root.insert(&mut txn, "schema_version", version);
        }
    }
    factory
        .ensure_schema(&mut doc, kind)
        .expect("mirror doc schema");
    doc
}

/// 读取镜像 doc 的 projection_version（§5.3 只读）。
fn projection_version(doc: &yrs::Doc) -> u32 {
    let txn = doc.transact();
    let Some(root) = crate::state::chat_writer::root_map_read(&txn) else {
        return 0;
    };
    root.get(&txn, "projection_version")
        .and_then(|v| v.cast::<u32>().ok())
        .unwrap_or(0)
}

/// apply update（yrs 编码版本 v1，§4.1）。
fn apply_update(doc: &yrs::Doc, update: &[u8]) {
    use yrs::updates::decoder::Decode as _;
    match yrs::Update::decode_v1(update) {
        Ok(parsed) => {
            let mut txn = doc.transact_mut();
            if let Err(e) = txn.apply_update(parsed) {
                warn!(error = ?e, "mirror update apply failed; skipped");
            }
        }
        Err(e) => {
            warn!(error = ?e, "mirror update decode failed; skipped");
        }
    }
}

/// Rebuild the visible Registry value graph in a fresh Yjs document.
///
/// A regular state update retains every historical item and tombstone. The
/// Registry has a server-authoritative, map-shaped schema, so carrying that
/// history forever only makes restart and initial subscription progressively
/// slower. `AsPrelim` recursively copies the currently visible shared values
/// while assigning fresh CRDT identities; old updates remain recoverable in
/// `registry.log.legacy-v1` during the compatibility window.
fn materialize_registry_update(factory: &Factory, source: &yrs::Doc) -> Result<Vec<u8>, HubError> {
    let values = {
        let txn = source.transact();
        let root = txn
            .get_map(crate::state::factory::ROOT)
            .ok_or_else(|| HubError::Store("registry snapshot has no root map".into()))?;
        root.iter(&txn)
            .map(|(key, value)| (key.to_string(), value.as_prelim(&txn)))
            .collect::<Vec<_>>()
    };
    let mut materialized = yrs::Doc::new();
    {
        use yrs::WriteTxn;
        let mut txn = materialized.transact_mut();
        let root = txn.get_or_insert_map(crate::state::factory::ROOT);
        for (key, value) in values {
            root.insert(&mut txn, key, value);
        }
    }
    factory
        .ensure_schema(&mut materialized, DocKind::Registry)
        .map_err(|error| HubError::Store(format!("materialize registry schema: {error}")))?;
    Ok(encode_state_as_update(&materialized))
}

/// registry 日志追加（tmp → fsync → rename → 目录 fsync，§8.4 纪律）。
fn append_registry_record(path: &std::path::Path, update: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    set_private_file(&f)?;
    write_blob(&mut f, update)?;
    f.sync_data()?;
    Ok(())
}

/// registry 日志重放（损坏尾部截断 + 告警，§8.4）。
struct RegistryRecovery {
    doc: Option<yrs::Doc>,
    records: usize,
}

fn recover_registry_doc(
    factory: &Factory,
    snapshot: &std::path::Path,
    log: &std::path::Path,
) -> Result<RegistryRecovery, HubError> {
    let mut updates = Vec::new();
    if snapshot.exists() {
        let mut file = File::open(snapshot).map_err(|e| HubError::Store(e.to_string()))?;
        match read_blob(&mut file) {
            Ok(Some(body)) => updates.push(body),
            Ok(None) => return Err(HubError::Store("registry snapshot is empty".into())),
            Err(error) => {
                return Err(HubError::Store(format!(
                    "registry snapshot corrupt: {error:?}"
                )))
            }
        }
    }
    let mut records = 0usize;
    if log.exists() {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(log)
            .map_err(|e| HubError::Store(e.to_string()))?;
        set_private_file(&file).map_err(|e| HubError::Store(e.to_string()))?;
        let file_len = file
            .metadata()
            .map_err(|e| HubError::Store(e.to_string()))?
            .len();
        loop {
            let valid_end = file
                .stream_position()
                .map_err(|e| HubError::Store(e.to_string()))?;
            match read_blob(&mut file) {
                Ok(Some(body)) => {
                    updates.push(body);
                    records += 1;
                }
                Ok(None) if valid_end == file_len => break,
                Ok(None) => {
                    warn!(path = %log.display(), valid_end, "registry log partial header truncated");
                    file.set_len(valid_end)
                        .map_err(|e| HubError::Store(e.to_string()))?;
                    break;
                }
                Err(error) => {
                    warn!(path = %log.display(), valid_end, error = ?error, "registry log corrupt tail truncated");
                    file.set_len(valid_end)
                        .map_err(|e| HubError::Store(e.to_string()))?;
                    break;
                }
            }
        }
    }
    let doc = if updates.is_empty() {
        None
    } else {
        Some(create_mirror_doc(factory, DocKind::Registry, &updates))
    };
    Ok(RegistryRecovery { doc, records })
}

fn compact_registry_log(
    snapshot: &std::path::Path,
    tmp: &std::path::Path,
    log: &std::path::Path,
    legacy_backup: &std::path::Path,
    update: &[u8],
) -> std::io::Result<()> {
    let dir = snapshot
        .parent()
        .ok_or_else(|| std::io::Error::other("registry snapshot has no parent"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(tmp)?;
    set_private_file(&file)?;
    write_blob(&mut file, update)?;
    file.sync_all()?;
    std::fs::rename(tmp, snapshot)?;
    sync_dir(dir).map_err(|e| std::io::Error::other(e.to_string()))?;
    // Read-after-rename validates length, CRC and Yjs decoding before the only
    // destructive step (log truncation).
    let mut verify = File::open(snapshot)?;
    let body = read_blob(&mut verify)
        .map_err(|e| std::io::Error::other(format!("snapshot verify: {e:?}")))?
        .ok_or_else(|| std::io::Error::other("snapshot verify: empty"))?;
    yrs::Update::decode_v1(&body)
        .map_err(|e| std::io::Error::other(format!("snapshot Yjs decode: {e:?}")))?;
    let first_snapshot =
        !legacy_backup.exists() && std::fs::metadata(log).map(|m| m.len() > 0).unwrap_or(false);
    if first_snapshot {
        std::fs::rename(log, legacy_backup)?;
        let backup = File::open(legacy_backup)?;
        set_private_file(&backup)?;
        backup.sync_all()?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log)?;
    set_private_file(&log_file)?;
    log_file.sync_all()?;
    sync_dir(dir).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

fn set_private_file(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "hub_test.rs"]
mod hub_test;
