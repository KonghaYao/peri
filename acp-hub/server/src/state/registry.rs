//! Registry Doc 写入者（server 状态源单写接口，§5.2/§5.5/§17.2）。
//!
//! Registry Doc（`hub:registry`）是 TUI 会话列表与机器列表的**唯一权威源**：
//! 活跃 chat 摘要由 server 状态源单写（chat 生命周期事件驱动：
//! create/binding/终态/close 时更新），**不从 chat Doc 聚合**（§5.2 裁决）。
//! 聚合器不直写 Registry Doc（gap 经上报路径，§9.4）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use yrs::{Map, ReadTxn, Transact, WriteTxn};

use acp_hub_proto::schema::{
    ChatSummary, GlobalStatus, InstanceStatus, InstanceView, ProjectSessionSummary, ProjectSummary,
    SessionSummaryProjection, WorkspaceSummary,
};

use crate::state::doc_manager::DocCommand;
use crate::state::factory::ROOT;
use crate::state::session_list;

/// Registry 写者命令（Registry Doc 无 chat 维度：低频、无微批次、即到即写，
/// §8.5【决策】路由到全局 registry 写者）。
pub(crate) enum RegistryMsg {
    /// 通用命令（§8.5 Registry 系）。
    Command(DocCommand, oneshot::Sender<Result<(), RegistryError>>),
    /// gap 写回（§9.4/§12.4）：`Some(count)` 置缺口、`None` 追平清除。
    SetChatGap {
        chat_id: String,
        gap: Option<u64>,
        reply: oneshot::Sender<Result<(), RegistryError>>,
    },
    /// chat 状态迁移写回（§7.3/§7.6）。
    SetChatStatus {
        chat_id: String,
        status: String,
        reply: oneshot::Sender<Result<(), RegistryError>>,
    },
    /// workspace 全量查询（启动恢复：内存注册表从 Registry Doc 重建）。
    ListWorkspaces(oneshot::Sender<Vec<WorkspaceSummary>>),
    ListLegacySessions(oneshot::Sender<Vec<SessionSummaryProjection>>),
}

/// Registry 操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// 写者已退出（channel closed）。
    #[error("registry writer closed")]
    ChannelClosed,
    /// 目标条目不存在（如 set_instance_status 的 instance 未注册）。
    #[error("registry entry not found: {0}")]
    NotFound(String),
}

/// Degraded 判定输入（§17.2：任一触发 Degraded）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DegradeCause {
    /// 落盘失败（F6 上报）。
    PersistFailure,
    /// 缓冲溢出丢弃（channel 层上报，§8.5）。
    BufferDropped,
    /// 任一存活 chat 存在 gap（聚合器上报，§9.4）。
    ChatGap,
    /// 镜像失败（聚合器/writer task 异常，§17.2）。
    ProjectionError,
    /// 启动恢复不变量失败（§8.4.1，F6/恢复流程上报）。
    RestoreInvariant,
}

impl DegradeCause {
    fn label(self) -> &'static str {
        match self {
            DegradeCause::PersistFailure => "persist_failure",
            DegradeCause::BufferDropped => "buffer_dropped",
            DegradeCause::ChatGap => "chat_gap",
            DegradeCause::ProjectionError => "projection_error",
            DegradeCause::RestoreInvariant => "restore_invariant",
        }
    }
}

/// Registry Doc 写入者（server 状态源单写接口，§5.2）。
///
/// 内部经 DocManager 全局 registry 写者执行（§8.5 命令路由）；调用方为
/// channel 层（instance 生命周期，F7/F8）与恢复流程（F6）。
#[derive(Clone)]
pub struct RegistryState {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    tx: mpsc::Sender<RegistryMsg>,
    /// 活跃 degraded 条件集合（§12.3 判定集中）。
    conditions: Mutex<HashSet<DegradeCause>>,
    /// server 启动回放期标志（§8.4.1）。
    restarting: AtomicBool,
}

impl RegistryState {
    pub(crate) fn new(tx: mpsc::Sender<RegistryMsg>) -> Self {
        RegistryState {
            inner: Arc::new(RegistryInner {
                tx,
                conditions: Mutex::new(HashSet::new()),
                restarting: AtomicBool::new(false),
            }),
        }
    }

    /// instance 视图 upsert（hello 注册/心跳/offline；§7.1）。
    pub async fn upsert_instance(&self, m: InstanceView) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryUpsertInstance(m)).await
    }

    /// instance 状态更新（online/offline/unknown；心跳超时驱动）。
    pub async fn set_instance_status(
        &self,
        instance_id: &str,
        status: InstanceStatus,
    ) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistrySetInstanceState {
            instance_id: instance_id.to_string(),
            status,
        })
        .await
    }

    /// 活跃 chat 摘要 upsert（create/binding 建立/标题/终态/gap 同步）。
    pub async fn upsert_chat(&self, s: ChatSummary) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryUpsertChat(s)).await
    }

    /// 移除（chat close 清理）。
    pub async fn remove_chat(&self, chat_id: &str) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryRemoveChat {
            chat_id: chat_id.to_string(),
        })
        .await
    }

    /// gap 写回（聚合器上报，§9.4）：`Some(count)` 置缺口、`None` 追平清除。
    pub async fn set_chat_gap(&self, chat_id: &str, gap: Option<u64>) -> Result<(), RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(RegistryMsg::SetChatGap {
                chat_id: chat_id.to_string(),
                gap,
                reply,
            })
            .await
            .map_err(|_| RegistryError::ChannelClosed)?;
        rx.await.map_err(|_| RegistryError::ChannelClosed)?
    }

    /// chat 状态迁移（accepting/active/ended/closed/crashed + pending_close，
    /// §7.3/§7.6）。
    pub async fn set_chat_status(&self, chat_id: &str, status: &str) -> Result<(), RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(RegistryMsg::SetChatStatus {
                chat_id: chat_id.to_string(),
                status: status.to_string(),
                reply,
            })
            .await
            .map_err(|_| RegistryError::ChannelClosed)?;
        rx.await.map_err(|_| RegistryError::ChannelClosed)?
    }

    /// 全局状态（§17.2）：条件上报，任一 cause 活跃 → Degraded。
    pub async fn report_condition(&self, cause: DegradeCause) -> Result<(), RegistryError> {
        let was_empty = {
            let mut conditions = self.inner.conditions.lock().unwrap();
            let was_empty = conditions.is_empty();
            conditions.insert(cause);
            was_empty
        };
        if was_empty && !self.inner.restarting.load(Ordering::SeqCst) {
            self.set_global(GlobalStatus::Degraded).await?;
        }
        tracing::warn!(
            cause = cause.label(),
            degraded = true,
            "degraded condition reported"
        );
        Ok(())
    }

    /// 全局状态（§17.2）：条件清除；全部清除 → Healthy。
    pub async fn clear_condition(&self, cause: DegradeCause) -> Result<(), RegistryError> {
        let empty = {
            let mut conditions = self.inner.conditions.lock().unwrap();
            conditions.remove(&cause);
            conditions.is_empty()
        };
        if empty && !self.inner.restarting.load(Ordering::SeqCst) {
            self.set_global(GlobalStatus::Healthy).await?;
        }
        tracing::info!(cause = cause.label(), "degraded condition cleared");
        Ok(())
    }

    /// 启动回放期置 Restarting（§8.4.1；恢复流程显式调用）。
    pub async fn set_restarting(&self) -> Result<(), RegistryError> {
        self.inner.restarting.store(true, Ordering::SeqCst);
        self.set_global(GlobalStatus::Restarting).await
    }

    /// 恢复完成置出 Restarting（§8.4.1【决策】补充：设计稿 §12.2 仅有置入，
    /// 「恢复不变量完成置 Healthy」需要置出接口）。
    pub async fn clear_restarting(&self) -> Result<(), RegistryError> {
        self.inner.restarting.store(false, Ordering::SeqCst);
        let degraded = !self.inner.conditions.lock().unwrap().is_empty();
        let status = if degraded {
            GlobalStatus::Degraded
        } else {
            GlobalStatus::Healthy
        };
        self.set_global(status).await
    }

    /// 当前判定状态（读 Registry Doc global.status 的镜像；条件集合为空且非
    /// restarting → Healthy，否则 Degraded）。供 F7 消费（拒绝新 committed）。
    pub fn global_status(&self) -> GlobalStatus {
        if self.inner.restarting.load(Ordering::SeqCst) {
            return GlobalStatus::Restarting;
        }
        if self.inner.conditions.lock().unwrap().is_empty() {
            GlobalStatus::Healthy
        } else {
            GlobalStatus::Degraded
        }
    }

    /// ACP `session/list` 全量同步投影（§6.3，instance 级数据 → Registry
    /// Doc `sessions` map；幂等 + 自愈删除）。轮询器每 instance 一轮调一次。
    pub async fn apply_sessions(
        &self,
        entries: Vec<SessionSummaryProjection>,
    ) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryApplySessions { entries })
            .await
    }

    /// 工作区摘要 upsert（create/rename；Registry Doc `workspaces` map）。
    pub async fn upsert_workspace(&self, w: WorkspaceSummary) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryUpsertWorkspace(w)).await
    }

    /// 工作区移除（Registry Doc `workspaces` map 删键；已建对话不受影响）。
    pub async fn remove_workspace(&self, workspace_id: &str) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryRemoveWorkspace {
            workspace_id: workspace_id.to_string(),
        })
        .await
    }

    /// 工作区全量查询（启动恢复用：workspace 内存注册表从 Registry Doc 重建，
    /// 保证重启后 create 继承 cwd 仍可用）。
    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceSummary>, RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(RegistryMsg::ListWorkspaces(reply))
            .await
            .map_err(|_| RegistryError::ChannelClosed)?;
        rx.await.map_err(|_| RegistryError::ChannelClosed)
    }

    pub async fn list_legacy_sessions(
        &self,
    ) -> Result<Vec<SessionSummaryProjection>, RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(RegistryMsg::ListLegacySessions(reply))
            .await
            .map_err(|_| RegistryError::ChannelClosed)?;
        rx.await.map_err(|_| RegistryError::ChannelClosed)
    }

    pub async fn replace_projects(
        &self,
        projects: Vec<ProjectSummary>,
        sessions: Vec<ProjectSessionSummary>,
    ) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistryReplaceProjects { projects, sessions })
            .await
    }

    async fn set_global(&self, status: GlobalStatus) -> Result<(), RegistryError> {
        self.send(DocCommand::RegistrySetGlobal { status }).await
    }

    async fn send(&self, cmd: DocCommand) -> Result<(), RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(RegistryMsg::Command(cmd, reply))
            .await
            .map_err(|_| RegistryError::ChannelClosed)?;
        rx.await.map_err(|_| RegistryError::ChannelClosed)?
    }
}

// ---------------------------------------------------------------------------
// registry 写者 task（DocManager spawn；唯一提交边界内的全局写者）
// ---------------------------------------------------------------------------

/// Registry Doc 写者执行体：应用命令到 Registry Doc。
pub(crate) struct RegistryApplier {
    pub(crate) doc: yrs::Doc,
}

impl RegistryApplier {
    pub(crate) fn new(doc: yrs::Doc) -> Self {
        RegistryApplier { doc }
    }

    /// 执行一条 registry 命令。
    pub(crate) fn apply(&mut self, cmd: &DocCommand) -> Result<(), RegistryError> {
        let mut txn = self.doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        match cmd {
            DocCommand::RegistryUpsertInstance(m) => {
                write_instance(&mut txn, &root, m);
                Ok(())
            }
            DocCommand::RegistrySetInstanceState {
                instance_id,
                status,
            } => {
                let instances = root.get_or_init::<_, yrs::MapRef>(&mut txn, "instances");
                let Some(mm) = instances
                    .get(&txn, instance_id)
                    .and_then(|v| v.cast::<yrs::MapRef>().ok())
                else {
                    return Err(RegistryError::NotFound(instance_id.clone()));
                };
                mm.insert(&mut txn, "status", instance_status_str(*status));
                Ok(())
            }
            DocCommand::RegistryUpsertChat(s) => {
                write_chat_summary(&mut txn, &root, s);
                Ok(())
            }
            DocCommand::RegistryRemoveChat { chat_id } => {
                let chats = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chats");
                chats.remove(&mut txn, chat_id);
                Ok(())
            }
            DocCommand::RegistrySetGlobal { status } => {
                let global = root.get_or_init::<_, yrs::MapRef>(&mut txn, "global");
                global.insert(&mut txn, "status", global_status_str(*status));
                Ok(())
            }
            DocCommand::RegistryApplySessions { entries } => {
                // 与 chat 控制面 SessionListResponse 同构（§6.3）：预读当前
                // 投影 → diff → 有变化才写（幂等，无变化 no-op）。sessions
                // 是 instance 级数据，投影到 Registry Doc `sessions` map。
                let current = session_list::read_current(&txn, &root);
                let d = session_list::diff(&current, entries);
                if !d.upsert.is_empty() || !d.remove.is_empty() {
                    session_list::apply_diff(&mut txn, &root, &d);
                }
                Ok(())
            }
            DocCommand::RegistryUpsertWorkspace(w) => {
                write_workspace(&mut txn, &root, w);
                Ok(())
            }
            DocCommand::RegistryRemoveWorkspace { workspace_id } => {
                let workspaces = root.get_or_init::<_, yrs::MapRef>(&mut txn, "workspaces");
                workspaces.remove(&mut txn, workspace_id);
                Ok(())
            }
            DocCommand::RegistryReplaceProjects { projects, sessions } => {
                let pm = root.get_or_init::<_, yrs::MapRef>(&mut txn, "projects");
                pm.clear(&mut txn);
                for p in projects {
                    let m = pm.get_or_init::<_, yrs::MapRef>(&mut txn, p.id.as_str());
                    m.insert(&mut txn, "id", p.id.clone());
                    m.insert(&mut txn, "name", p.name.clone());
                    m.insert(&mut txn, "cwd", p.cwd.clone());
                    m.insert(&mut txn, "instance_id", p.instance_id.clone());
                    m.insert(&mut txn, "created_at", p.created_at.clone());
                    m.insert(&mut txn, "updated_at", p.updated_at.clone());
                    match &p.archived_at {
                        Some(v) => m.insert(&mut txn, "archived_at", v.clone()),
                        None => m.insert(&mut txn, "archived_at", yrs::Any::Null),
                    };
                }
                let sm = root.get_or_init::<_, yrs::MapRef>(&mut txn, "project_sessions");
                sm.clear(&mut txn);
                for s in sessions {
                    let m = sm.get_or_init::<_, yrs::MapRef>(&mut txn, s.id.as_str());
                    m.insert(&mut txn, "id", s.id.clone());
                    m.insert(&mut txn, "project_id", s.project_id.clone());
                    match &s.acp_session_id {
                        Some(v) => m.insert(&mut txn, "acp_session_id", v.clone()),
                        None => m.insert(&mut txn, "acp_session_id", yrs::Any::Null),
                    };
                    m.insert(&mut txn, "title", s.title.clone());
                    m.insert(&mut txn, "lifecycle", s.lifecycle.clone());
                    m.insert(&mut txn, "updated_at", s.updated_at.clone());
                    match &s.last_opened_at {
                        Some(v) => m.insert(&mut txn, "last_opened_at", v.clone()),
                        None => m.insert(&mut txn, "last_opened_at", yrs::Any::Null),
                    };
                    match &s.active_chat_id {
                        Some(v) => m.insert(&mut txn, "active_chat_id", v.clone()),
                        None => m.insert(&mut txn, "active_chat_id", yrs::Any::Null),
                    };
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// workspace 全量读取（启动恢复：workspace 内存注册表重建；读 doc 快照，
    /// 不经命令路径）。
    pub(crate) fn list_workspaces(&self) -> Vec<WorkspaceSummary> {
        let txn = self.doc.transact();
        let root = match txn.get_map(ROOT) {
            Some(r) => r,
            None => return Vec::new(),
        };
        let Some(ws) = root
            .get(&txn, "workspaces")
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
        else {
            return Vec::new();
        };
        let str_or = |m: &yrs::MapRef, k: &str| -> String {
            m.get(&txn, k)
                .and_then(|x| x.cast::<String>().ok())
                .unwrap_or_default()
        };
        let mut out = Vec::new();
        for (id, v) in ws.iter(&txn) {
            if let Ok(m) = v.cast::<yrs::MapRef>() {
                out.push(WorkspaceSummary {
                    id: id.to_string(),
                    name: str_or(&m, "name"),
                    cwd: str_or(&m, "cwd"),
                    created_at: str_or(&m, "created_at"),
                    updated_at: str_or(&m, "updated_at"),
                });
            }
        }
        out
    }

    pub(crate) fn list_legacy_sessions(&self) -> Vec<SessionSummaryProjection> {
        let txn = self.doc.transact();
        let Some(root) = txn.get_map(ROOT) else {
            return Vec::new();
        };
        session_list::read_current(&txn, &root)
            .into_values()
            .collect()
    }

    /// gap 写回（读现状改 gap 字段；§9.4/§12.4）。
    pub(crate) fn set_chat_gap(
        &mut self,
        chat_id: &str,
        gap: Option<u64>,
    ) -> Result<(), RegistryError> {
        let mut txn = self.doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        let chats = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chats");
        let Some(sm) = chats
            .get(&txn, chat_id)
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
        else {
            return Err(RegistryError::NotFound(chat_id.to_string()));
        };
        match gap {
            Some(count) => sm.insert(&mut txn, "gap", count as f64),
            None => sm.insert(&mut txn, "gap", yrs::Any::Null),
        };
        Ok(())
    }

    /// chat 状态迁移写回（读现状改 status；§7.3/§7.6）。
    pub(crate) fn set_chat_status(
        &mut self,
        chat_id: &str,
        status: &str,
    ) -> Result<(), RegistryError> {
        let mut txn = self.doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        let chats = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chats");
        let Some(sm) = chats
            .get(&txn, chat_id)
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
        else {
            return Err(RegistryError::NotFound(chat_id.to_string()));
        };
        sm.insert(&mut txn, "status", status.to_string());
        Ok(())
    }
}

fn write_instance(txn: &mut yrs::TransactionMut<'_>, root: &yrs::MapRef, m: &InstanceView) {
    let instances = root.get_or_init::<_, yrs::MapRef>(txn, "instances");
    let mm = instances.get_or_init::<_, yrs::MapRef>(txn, m.id.as_str());
    mm.insert(txn, "id", m.id.clone());
    mm.insert(txn, "hostname", m.hostname.clone());
    mm.insert(txn, "status", instance_status_str(m.status));
    mm.insert(txn, "token_id", m.token_id.clone());
    mm.insert(txn, "registered_at", m.registered_at.clone());
    mm.insert(txn, "last_heartbeat", m.last_heartbeat.clone());
    mm.insert(txn, "chat_count", m.chat_count as f64);
}

fn write_chat_summary(txn: &mut yrs::TransactionMut<'_>, root: &yrs::MapRef, s: &ChatSummary) {
    let chats = root.get_or_init::<_, yrs::MapRef>(txn, "chats");
    let sm = chats.get_or_init::<_, yrs::MapRef>(txn, s.id.as_str());
    // 重启语义（§5.5）：已存在条目（历史会话）不得被无条件覆盖——status
    // 由 set_chat_status 权威管理（否则客户端订阅触发的 open_chat
    // 会把启动对账标记的 ended 复活回 accepting，实测 836c8a3e）；title/
    // instance_id 同理（重启后内存表为空，覆盖会抹掉历史标题）。upsert
    // 收敛为：新条目全量写入；已存在条目仅刷新 updated_at（列表排序）。
    if sm.get(txn, "status").is_some() {
        sm.insert(txn, "updated_at", s.updated_at.clone());
        return;
    }
    sm.insert(txn, "id", s.id.clone());
    sm.insert(txn, "instance_id", s.instance_id.clone());
    sm.insert(txn, "title", s.title.clone());
    sm.insert(txn, "status", s.status.clone());
    match s.gap {
        Some(count) => sm.insert(txn, "gap", count as f64),
        None => sm.insert(txn, "gap", yrs::Any::Null),
    };
    sm.insert(txn, "updated_at", s.updated_at.clone());
    sm.insert(txn, "cwd", s.cwd.clone());
    match &s.workspace_id {
        Some(id) => sm.insert(txn, "workspace_id", id.clone()),
        None => sm.insert(txn, "workspace_id", yrs::Any::Null),
    };
}

fn write_workspace(txn: &mut yrs::TransactionMut<'_>, root: &yrs::MapRef, w: &WorkspaceSummary) {
    let workspaces = root.get_or_init::<_, yrs::MapRef>(txn, "workspaces");
    let wm = workspaces.get_or_init::<_, yrs::MapRef>(txn, w.id.as_str());
    wm.insert(txn, "id", w.id.clone());
    wm.insert(txn, "name", w.name.clone());
    wm.insert(txn, "cwd", w.cwd.clone());
    wm.insert(txn, "created_at", w.created_at.clone());
    wm.insert(txn, "updated_at", w.updated_at.clone());
}

pub(crate) fn instance_status_str(s: InstanceStatus) -> &'static str {
    match s {
        InstanceStatus::Online => "online",
        InstanceStatus::Offline => "offline",
        InstanceStatus::Unknown => "unknown",
    }
}

pub(crate) fn global_status_str(s: GlobalStatus) -> &'static str {
    match s {
        GlobalStatus::Healthy => "healthy",
        GlobalStatus::Degraded => "degraded",
        GlobalStatus::Restarting => "restarting",
    }
}

/// 读取 Registry Doc 的全局状态（读侧辅助；broadcaster/TUI 快照用）。
#[allow(dead_code)] // 预留：F7 broadcaster 快照用
pub(crate) fn read_global_status(doc: &yrs::Doc) -> GlobalStatus {
    let txn = doc.transact();
    let Some(root) = txn.get_map(ROOT) else {
        return GlobalStatus::Healthy;
    };
    let status = root
        .get(&txn, "global")
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .and_then(|g| g.get(&txn, "status"))
        .and_then(|s| s.cast::<String>().ok())
        .unwrap_or_default();
    match status.as_str() {
        "degraded" => GlobalStatus::Degraded,
        "restarting" => GlobalStatus::Restarting,
        _ => GlobalStatus::Healthy,
    }
}
