//! chat 注册表（架构 §7.3/§7.6/§8.3，设计稿 `f5-channel-control.md` §12）。
//!
//! chat 生命周期（§7.3）+ 可信 binding（§6.1：`session_id → chat_id`）+
//! pending_close（§7.6：offline 时 close 补发）+ 重连对账（§8.3 步骤 5：
//! alive_sessions 与 Registry 比对）。
//!
//! Registry Doc `chats.status` 的进程内镜像（状态写回经 `RegistryState`，
//! server 状态源单写，§5.2）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use acp_hub_proto::schema::ChatSummary;

use crate::state::registry::{RegistryError, RegistryState};

/// chat 级状态（§7.3/§7.6；`chats.status` 字符串映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatState {
    /// create 后、binding 前。
    Accepting,
    /// ACP 进程退出（终态，视图保留）。
    Ended,
    /// 用户关闭（终态）。
    Closed,
    /// 进程崩溃（终态）。
    Crashed,
    /// instance 分区（可恢复；补推追平后清除，§7.3）。
    Gap,
    /// close 遇 offline（§7.6；instance 重连后补发 kill）。
    PendingClose,
}

impl ChatState {
    /// Registry `chats.status` 字符串（§5.5 M1 透传）。
    pub fn as_str(self) -> &'static str {
        match self {
            ChatState::Accepting => "accepting",
            ChatState::Ended => "ended",
            ChatState::Closed => "closed",
            ChatState::Crashed => "crashed",
            ChatState::Gap => "gap",
            ChatState::PendingClose => "pending_close",
        }
    }

    /// 是否终态（ended/closed/crashed；§8.2「不再接受该 chat 的新事件」）。
    pub fn is_terminal(self) -> bool {
        matches!(self, ChatState::Ended | ChatState::Closed | ChatState::Crashed)
    }
}

/// chat 条目（进程内镜像）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRecord {
    /// chat 状态。
    pub state: ChatState,
    /// 归属 instance。
    pub instance_id: String,
    /// 标题。
    pub title: String,
    /// binding 建立后的 session_id（ACP 会话，§6.2）。
    pub session_id: Option<String>,
    /// ACP 进程工作目录（继承自 workspace 或 server 默认目录，§6.3
    /// workspace 扩展；session/list 轮询查询面）。
    pub cwd: String,
    /// 归属工作区（无 → None；工作区删除后已建对话保留此引用）。
    pub workspace_id: Option<String>,
    /// 创建时刻（server 权威时钟，§4.7）。
    pub created_at: DateTime<Utc>,
    /// 最近变更时刻。
    pub updated_at: DateTime<Utc>,
}

/// chat 操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    /// chat 未登记。
    #[error("chat not found: {0}")]
    NotFound(String),
    /// binding 冲突（session_id 已绑定到另一 chat；参数 = 已绑定的 chat_id）。
    #[error("binding conflict: session already bound to chat {0}")]
    BindingConflict(String),
    /// Registry 状态写回失败。
    #[error("registry write failed: {0}")]
    Registry(#[from] RegistryError),
}

/// 重连对账摘要（§8.3 步骤 5）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconciliationReport {
    /// instance 存活且 server 登记的 chat（正常）。
    pub alive: Vec<String>,
    /// instance 存活但 server 已标记终态/未知（应补发 kill，§7.5/§7.6）。
    pub unexpected_alive: Vec<String>,
    /// server 登记但 instance 未上报存活（进程已死；置 gap 或已由断链处理）。
    pub missing: Vec<String>,
    /// 对账后需要 server 下发 kill 的 chat（pending_close 补发 + 意外存活
    /// 裁决，§7.6/§8.3）。
    pub to_kill: Vec<String>,
}

/// chat 注册表（§7.3 状态机 + binding + pending_close + 对账）。
#[derive(Clone)]
pub struct ChatRegistry {
    inner: Arc<ChatInner>,
}

struct ChatInner {
    chats: RwLock<HashMap<String, ChatRecord>>,
    /// 可信 binding：session_id（ACP 会话）→ chat_id（§6.1 规则 5）。
    bindings: RwLock<HashMap<String, String>>,
    /// pending_close 补发集合（§7.6）。
    pending_close: RwLock<HashSet<String>>,
    /// 活动 turn 登记（chat → turn_id；断链清理输入，§7.1「活动 turn →
    /// interrupted」。coordinator 登记、断链清理消费）。
    active_turns: RwLock<HashMap<String, String>>,
    registry: RegistryState,
}

impl ChatRegistry {
    /// 以 Registry 状态源句柄构建。
    pub fn new(registry: RegistryState) -> Self {
        ChatRegistry {
            inner: Arc::new(ChatInner {
                chats: RwLock::new(HashMap::new()),
                bindings: RwLock::new(HashMap::new()),
                pending_close: RwLock::new(HashSet::new()),
                active_turns: RwLock::new(HashMap::new()),
                registry,
            }),
        }
    }

    /// Registry 状态源句柄（session/list 轮询投影等全局写入用）。
    pub fn registry(&self) -> RegistryState {
        self.inner.registry.clone()
    }

    /// create 登记（coordinator create 流程调用；状态 = Accepting）。
    ///
    /// Registry Doc 写回顺序（§5.2 单写）：先 `upsert_chat` 建立条目
    /// （Registry 读侧/TUI 对话列表的权威源），再 `set_chat_status` 迁移
    /// 状态——`set_chat_status` 要求条目已存在（state 层 applier 契约）。
    ///
    /// `cwd`：ACP 进程工作目录（继承自 workspace 或 server 默认目录，
    /// §6.3 workspace 扩展）；`workspace_id`：归属工作区（无 → None）。
    pub async fn register(
        &self,
        chat_id: &str,
        instance_id: &str,
        title: Option<&str>,
        cwd: &str,
        workspace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        let now = Utc::now();
        let title = title.unwrap_or_default();
        let entry = ChatRecord {
            state: ChatState::Accepting,
            instance_id: instance_id.to_string(),
            title: title.to_string(),
            session_id: None,
            cwd: cwd.to_string(),
            workspace_id: workspace_id.map(str::to_string),
            created_at: now,
            updated_at: now,
        };
        self.inner
            .chats
            .write()
            .await
            .insert(chat_id.to_string(), entry);
        // Registry 条目建立（upsert 幂等；chat_id 由 server 生成，无冲突）。
        self.inner
            .registry
            .upsert_chat(ChatSummary {
                id: chat_id.to_string(),
                instance_id: instance_id.to_string(),
                title: title.to_string(),
                status: ChatState::Accepting.as_str().to_string(),
                gap: None,
                updated_at: now.to_rfc3339(),
                cwd: cwd.to_string(),
                workspace_id: workspace_id.map(str::to_string),
            })
            .await?;
        self.inner
            .registry
            .set_chat_status(chat_id, ChatState::Accepting.as_str())
            .await?;
        debug!(chat_id, instance_id, "chat registered");
        Ok(())
    }

    /// binding 建立（§6.2：session/new 结果 → session_id → chat_id）。
    /// 此后该 chat 的 ACP 帧才允许投影（binding 前到达的帧一律丢弃，§6.2）。
    pub async fn bind(&self, chat_id: &str, session_id: &str) -> Result<(), ChatError> {
        let mut bindings = self.inner.bindings.write().await;
        if let Some(existing) = bindings.get(session_id) {
            if existing != chat_id {
                return Err(ChatError::BindingConflict(existing.clone()));
            }
            return Ok(()); // 幂等
        }
        bindings.insert(session_id.to_string(), chat_id.to_string());
        drop(bindings);
        let mut chats = self.inner.chats.write().await;
        if let Some(entry) = chats.get_mut(chat_id) {
            entry.session_id = Some(session_id.to_string());
            entry.updated_at = Utc::now();
        }
        info!(chat_id, session_id, "chat bound");
        Ok(())
    }

    /// binding 查询（RelayEventHandler/ACPChannel 投递前校验，§6.1 规则 5：
    /// session_id 只用于协议投递，不能成为 Doc 名/广播频道/缓存键）。
    pub async fn resolve(&self, session_id: &str) -> Option<String> {
        self.inner
            .bindings
            .read()
            .await
            .get(session_id)
            .cloned()
    }

    /// 会话切换（§8.5 当前对话内 load）：把 chat 的**当前会话**切到
    /// `session_id`（进程内切换，进程不重建）。新会话登记 binding
    /// （relay 逐帧校验需要命中——load 后事件帧携带新 sessionId），
    /// 旧会话 binding 保留（同 chat 映射无害，且旧会话可被切回——
    /// switch 幂等更新 entry.session_id，与 [`ChatRegistry::bind`] 的
    /// 幂等分支不同：bind 不更新已有绑定会话的 entry）。
    pub async fn switch_session(&self, chat_id: &str, session_id: &str) -> Result<(), ChatError> {
        {
            let mut bindings = self.inner.bindings.write().await;
            if let Some(existing) = bindings.get(session_id) {
                if existing != chat_id {
                    return Err(ChatError::BindingConflict(existing.clone()));
                }
            }
            bindings.insert(session_id.to_string(), chat_id.to_string());
        }
        let mut chats = self.inner.chats.write().await;
        if let Some(entry) = chats.get_mut(chat_id) {
            entry.session_id = Some(session_id.to_string());
            entry.updated_at = Utc::now();
        }
        info!(chat_id, session_id, "chat session switched (load)");
        Ok(())
    }

    /// 会话条目查询。
    pub async fn entry(&self, chat_id: &str) -> Option<ChatRecord> {
        self.inner.chats.read().await.get(chat_id).cloned()
    }

    /// instance offline 时的 close（§7.6）：返回 pending_close 标记（Registry
    /// 状态写回）；kill 补发由重连对账完成。
    pub async fn request_close_offline(&self, chat_id: &str) -> Result<(), ChatError> {
        let mut chats = self.inner.chats.write().await;
        let Some(entry) = chats.get_mut(chat_id) else {
            return Err(ChatError::NotFound(chat_id.to_string()));
        };
        entry.state = ChatState::PendingClose;
        entry.updated_at = Utc::now();
        self.inner
            .pending_close
            .write()
            .await
            .insert(chat_id.to_string());
        drop(chats);
        self.inner
            .registry
            .set_chat_status(chat_id, ChatState::PendingClose.as_str())
            .await?;
        warn!(chat_id, "chat close deferred: instance offline (pending_close)");
        Ok(())
    }

    /// 状态迁移 + Registry 写回（§7.3/§7.6；终态不可逆，防御性检查）。
    pub async fn transition(
        &self,
        chat_id: &str,
        state: ChatState,
    ) -> Result<(), ChatError> {
        let mut chats = self.inner.chats.write().await;
        let Some(entry) = chats.get_mut(chat_id) else {
            return Err(ChatError::NotFound(chat_id.to_string()));
        };
        if entry.state.is_terminal() && entry.state != state {
            warn!(
                chat_id, from = entry.state.as_str(), to = state.as_str(),
                "chat terminal state transition rejected (防御)"
            );
            return Ok(());
        }
        entry.state = state;
        entry.updated_at = Utc::now();
        if state == ChatState::Closed {
            // pending_close 完成：补发集合清除（§7.6）。
            self.inner.pending_close.write().await.remove(chat_id);
        }
        // 终态前取走 session_id（drop 后不可用；仅终态需要）。
        let bound_session = if state.is_terminal() {
            entry.session_id.clone()
        } else {
            None
        };
        drop(chats);
        // 终态：释放 binding（§8.5 激活语义）——对话关闭/崩溃后其 ACP
        // 会话不再被占用，可被再次激活/加载（否则 bindings 永不清理，
        // 会话重启前永远冲突）。
        if let Some(sid) = bound_session {
            let mut bindings = self.inner.bindings.write().await;
            if bindings.get(&sid).map(String::as_str) == Some(chat_id) {
                bindings.remove(&sid);
                debug!(chat_id, session_id = %sid, "binding released (terminal)");
            }
        }
        self.inner
            .registry
            .set_chat_status(chat_id, state.as_str())
            .await?;
        debug!(chat_id, state = state.as_str(), "chat state transitioned");
        Ok(())
    }

    /// 该 instance 的全部活 chat（断链清理输入，§8.2 matrix instance 行）。
    pub async fn chats_for_instance(&self, instance_id: &str) -> Vec<(String, ChatState)> {
        self.inner
            .chats
            .read()
            .await
            .iter()
            .filter(|(_, e)| e.instance_id == instance_id)
            .map(|(id, e)| (id.clone(), e.state))
            .collect()
    }

    /// 全部 chat 条目快照（session 轮询输入，§6.3；含终态，筛选由调用方）。
    pub async fn all_chats(&self) -> Vec<(String, ChatRecord)> {
        self.inner
            .chats
            .read()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), e.clone()))
            .collect()
    }

    /// 重连对账（§8.3 步骤 5）：alive_sessions 与 Registry 比对 → 摘要 +
    /// 待 kill 清单（意外存活 §7.5 裁决 + pending_close 补发 §7.6）。
    pub async fn reconcile_alive(
        &self,
        instance_id: &str,
        alive: &[String],
    ) -> Result<ReconciliationReport, ChatError> {
        let chats = self.inner.chats.read().await;
        let mut report = ReconciliationReport::default();
        let registered: HashMap<String, ChatRecord> = chats
            .iter()
            .filter(|(_, e)| e.instance_id == instance_id)
            .map(|(id, e)| (id.clone(), e.clone()))
            .collect();
        let pending_close = self.inner.pending_close.read().await.clone();
        drop(chats);

        for cid in alive {
            match registered.get(cid) {
                Some(e) if e.state.is_terminal() => {
                    // server 已标记终态但 instance 声称存活 → 意外存活，kill 裁决
                    // （§7.5）。
                    report.unexpected_alive.push(cid.clone());
                    report.to_kill.push(cid.clone());
                }
                Some(_) => report.alive.push(cid.clone()),
                None => {
                    // server 无登记（重启后遗留）→ 意外存活，kill 清理。
                    report.unexpected_alive.push(cid.clone());
                    report.to_kill.push(cid.clone());
                }
            }
        }
        for cid in registered.keys() {
            if !alive.contains(cid) {
                report.missing.push(cid.clone());
            }
        }
        // pending_close 补发（§7.6：instance 重连后自动补发 kill）。
        for cid in &pending_close {
            if !report.to_kill.contains(cid) {
                report.to_kill.push(cid.clone());
            }
        }
        info!(
            instance_id,
            alive = report.alive.len(),
            unexpected_alive = report.unexpected_alive.len(),
            missing = report.missing.len(),
            to_kill = report.to_kill.len(),
            "alive_sessions reconciliation complete"
        );
        Ok(report)
    }

    /// pending_close 集合快照（诊断/测试）。
    pub async fn pending_close_chats(&self) -> Vec<String> {
        self.inner
            .pending_close
            .read()
            .await
            .iter()
            .cloned()
            .collect()
    }

    /// binding 反向查询（chat_id → 绑定的 session_id；diagnostics）。
    pub async fn session_id(&self, chat_id: &str) -> Option<String> {
        self.inner
            .chats
            .read()
            .await
            .get(chat_id)
            .and_then(|e| e.session_id.clone())
    }

    /// 活动 turn 登记（coordinator prompt 执行登记；§7.1 断链清理输入）。
    pub async fn set_active_turn(&self, chat_id: &str, turn_id: &str) {
        self.inner
            .active_turns
            .write()
            .await
            .insert(chat_id.to_string(), turn_id.to_string());
    }

    /// 活动 turn 清除（turn 终态 / chat 关闭 / 断链清理后）。
    pub async fn clear_active_turn(&self, chat_id: &str) {
        self.inner.active_turns.write().await.remove(chat_id);
    }

    /// 活动 turn 查询（断链清理：`MarkTurnInterrupted` 输入，§7.1）。
    pub async fn active_turn(&self, chat_id: &str) -> Option<String> {
        self.inner.active_turns.read().await.get(chat_id).cloned()
    }
}

#[cfg(test)]
#[path = "chat_registry_test.rs"]
mod chat_registry_test;
