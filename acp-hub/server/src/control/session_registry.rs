//! 会话注册表（架构 §7.3/§7.6/§8.3，设计稿 `f5-channel-control.md` §12）。
//!
//! 会话生命周期（§7.3）+ 可信 binding（§6.1：`acp_session_id → hub
//! session_id`）+ pending_close（§7.6：offline 时 close 补发）+ 重连对账
//! （§8.3 步骤 5：alive_sessions 与 Registry 比对）。
//!
//! Registry Doc `sessions.status` 的进程内镜像（状态写回经 `RegistryState`，
//! server 状态源单写，§5.2）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use acp_hub_proto::schema::SessionSummary;

use crate::state::registry::{RegistryError, RegistryState};

/// 会话级状态（§7.3/§7.6；`sessions.status` 字符串映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// create 后、binding 前。
    Accepting,
    /// ACP 进程退出（终态，视图保留）。
    Ended,
    /// 用户关闭（终态）。
    Closed,
    /// 进程崩溃（终态）。
    Crashed,
    /// machine 分区（可恢复；补推追平后清除，§7.3）。
    Gap,
    /// close 遇 offline（§7.6；machine 重连后补发 kill）。
    PendingClose,
}

impl SessionState {
    /// Registry `sessions.status` 字符串（§5.5 M1 透传）。
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Accepting => "accepting",
            SessionState::Ended => "ended",
            SessionState::Closed => "closed",
            SessionState::Crashed => "crashed",
            SessionState::Gap => "gap",
            SessionState::PendingClose => "pending_close",
        }
    }

    /// 是否终态（ended/closed/crashed；§8.2「不再接受该 session 的新事件」）。
    pub fn is_terminal(self) -> bool {
        matches!(self, SessionState::Ended | SessionState::Closed | SessionState::Crashed)
    }
}

/// 会话条目（进程内镜像）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    /// 会话状态。
    pub state: SessionState,
    /// 归属 machine。
    pub machine_id: String,
    /// 标题。
    pub title: String,
    /// binding 建立前的 acp_session_id（§6.2）。
    pub acp_session_id: Option<String>,
    /// 创建时刻（server 权威时钟，§4.7）。
    pub created_at: DateTime<Utc>,
    /// 最近变更时刻。
    pub updated_at: DateTime<Utc>,
}

/// 会话操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// session 未登记。
    #[error("session not found: {0}")]
    NotFound(String),
    /// binding 冲突（acp_session_id 已绑定到另一 session）。
    #[error("binding conflict: acp session {0} already bound")]
    BindingConflict(String),
    /// Registry 状态写回失败。
    #[error("registry write failed: {0}")]
    Registry(#[from] RegistryError),
}

/// 重连对账摘要（§8.3 步骤 5）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconciliationReport {
    /// machine 存活且 server 登记的 session（正常）。
    pub alive: Vec<String>,
    /// machine 存活但 server 已标记终态/未知（应补发 kill，§7.5/§7.6）。
    pub unexpected_alive: Vec<String>,
    /// server 登记但 machine 未上报存活（进程已死；置 gap 或已由断链处理）。
    pub missing: Vec<String>,
    /// 对账后需要 server 下发 kill 的 session（pending_close 补发 + 意外存活
    /// 裁决，§7.6/§8.3）。
    pub to_kill: Vec<String>,
}

/// 会话注册表（§7.3 状态机 + binding + pending_close + 对账）。
#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    sessions: RwLock<HashMap<String, SessionEntry>>,
    /// 可信 binding：acp_session_id → hub session_id（§6.1 规则 5）。
    bindings: RwLock<HashMap<String, String>>,
    /// pending_close 补发集合（§7.6）。
    pending_close: RwLock<HashSet<String>>,
    /// 活动 turn 登记（session → turn_id；断链清理输入，§7.1「活动 turn →
    /// interrupted」。coordinator 登记、断链清理消费）。
    active_turns: RwLock<HashMap<String, String>>,
    registry: RegistryState,
}

impl SessionRegistry {
    /// 以 Registry 状态源句柄构建。
    pub fn new(registry: RegistryState) -> Self {
        SessionRegistry {
            inner: Arc::new(SessionInner {
                sessions: RwLock::new(HashMap::new()),
                bindings: RwLock::new(HashMap::new()),
                pending_close: RwLock::new(HashSet::new()),
                active_turns: RwLock::new(HashMap::new()),
                registry,
            }),
        }
    }

    /// create 登记（coordinator create 流程调用；状态 = Accepting）。
    ///
    /// Registry Doc 写回顺序（§5.2 单写）：先 `upsert_session` 建立条目
    /// （Registry 读侧/TUI 会话列表的权威源），再 `set_session_status` 迁移
    /// 状态——`set_session_status` 要求条目已存在（state 层 applier 契约）。
    pub async fn register(
        &self,
        session_id: &str,
        machine_id: &str,
        title: Option<&str>,
    ) -> Result<(), SessionError> {
        let now = Utc::now();
        let title = title.unwrap_or_default();
        let entry = SessionEntry {
            state: SessionState::Accepting,
            machine_id: machine_id.to_string(),
            title: title.to_string(),
            acp_session_id: None,
            created_at: now,
            updated_at: now,
        };
        self.inner
            .sessions
            .write()
            .await
            .insert(session_id.to_string(), entry);
        // Registry 条目建立（upsert 幂等；session_id 由 server 生成，无冲突）。
        self.inner
            .registry
            .upsert_session(SessionSummary {
                id: session_id.to_string(),
                machine_id: machine_id.to_string(),
                title: title.to_string(),
                status: SessionState::Accepting.as_str().to_string(),
                gap: None,
                updated_at: now.to_rfc3339(),
            })
            .await?;
        self.inner
            .registry
            .set_session_status(session_id, SessionState::Accepting.as_str())
            .await?;
        debug!(session_id, machine_id, "session registered");
        Ok(())
    }

    /// binding 建立（§6.2：session/new 结果 → acp_session_id → session_id）。
    /// 此后该 session 的 ACP 帧才允许投影（binding 前到达的帧一律丢弃，§6.2）。
    pub async fn bind(&self, session_id: &str, acp_session_id: &str) -> Result<(), SessionError> {
        let mut bindings = self.inner.bindings.write().await;
        if let Some(existing) = bindings.get(acp_session_id) {
            if existing != session_id {
                return Err(SessionError::BindingConflict(acp_session_id.to_string()));
            }
            return Ok(()); // 幂等
        }
        bindings.insert(acp_session_id.to_string(), session_id.to_string());
        drop(bindings);
        let mut sessions = self.inner.sessions.write().await;
        if let Some(entry) = sessions.get_mut(session_id) {
            entry.acp_session_id = Some(acp_session_id.to_string());
            entry.updated_at = Utc::now();
        }
        info!(session_id, acp_session_id, "session bound");
        Ok(())
    }

    /// binding 查询（RelayEventHandler/ACPChannel 投递前校验，§6.1 规则 5：
    /// acp_session_id 只用于协议投递，不能成为 Doc 名/广播频道/缓存键）。
    pub async fn resolve(&self, acp_session_id: &str) -> Option<String> {
        self.inner
            .bindings
            .read()
            .await
            .get(acp_session_id)
            .cloned()
    }

    /// 会话条目查询。
    pub async fn entry(&self, session_id: &str) -> Option<SessionEntry> {
        self.inner.sessions.read().await.get(session_id).cloned()
    }

    /// machine offline 时的 close（§7.6）：返回 pending_close 标记（Registry
    /// 状态写回）；kill 补发由重连对账完成。
    pub async fn request_close_offline(&self, session_id: &str) -> Result<(), SessionError> {
        let mut sessions = self.inner.sessions.write().await;
        let Some(entry) = sessions.get_mut(session_id) else {
            return Err(SessionError::NotFound(session_id.to_string()));
        };
        entry.state = SessionState::PendingClose;
        entry.updated_at = Utc::now();
        self.inner
            .pending_close
            .write()
            .await
            .insert(session_id.to_string());
        drop(sessions);
        self.inner
            .registry
            .set_session_status(session_id, SessionState::PendingClose.as_str())
            .await?;
        warn!(session_id, "session close deferred: machine offline (pending_close)");
        Ok(())
    }

    /// 状态迁移 + Registry 写回（§7.3/§7.6；终态不可逆，防御性检查）。
    pub async fn transition(
        &self,
        session_id: &str,
        state: SessionState,
    ) -> Result<(), SessionError> {
        let mut sessions = self.inner.sessions.write().await;
        let Some(entry) = sessions.get_mut(session_id) else {
            return Err(SessionError::NotFound(session_id.to_string()));
        };
        if entry.state.is_terminal() && entry.state != state {
            warn!(
                session_id, from = entry.state.as_str(), to = state.as_str(),
                "session terminal state transition rejected (防御)"
            );
            return Ok(());
        }
        entry.state = state;
        entry.updated_at = Utc::now();
        if state == SessionState::Closed {
            // pending_close 完成：补发集合清除（§7.6）。
            self.inner.pending_close.write().await.remove(session_id);
        }
        drop(sessions);
        self.inner
            .registry
            .set_session_status(session_id, state.as_str())
            .await?;
        debug!(session_id, state = state.as_str(), "session state transitioned");
        Ok(())
    }

    /// 该 machine 的全部活 session（断链清理输入，§8.2 matrix machine 行）。
    pub async fn sessions_for_machine(&self, machine_id: &str) -> Vec<(String, SessionState)> {
        self.inner
            .sessions
            .read()
            .await
            .iter()
            .filter(|(_, e)| e.machine_id == machine_id)
            .map(|(id, e)| (id.clone(), e.state))
            .collect()
    }

    /// 重连对账（§8.3 步骤 5）：alive_sessions 与 Registry 比对 → 摘要 +
    /// 待 kill 清单（意外存活 §7.5 裁决 + pending_close 补发 §7.6）。
    pub async fn reconcile_alive(
        &self,
        machine_id: &str,
        alive: &[String],
    ) -> Result<ReconciliationReport, SessionError> {
        let sessions = self.inner.sessions.read().await;
        let mut report = ReconciliationReport::default();
        let registered: HashMap<String, SessionEntry> = sessions
            .iter()
            .filter(|(_, e)| e.machine_id == machine_id)
            .map(|(id, e)| (id.clone(), e.clone()))
            .collect();
        let pending_close = self.inner.pending_close.read().await.clone();
        drop(sessions);

        for sid in alive {
            match registered.get(sid) {
                Some(e) if e.state.is_terminal() => {
                    // server 已标记终态但 machine 声称存活 → 意外存活，kill 裁决
                    // （§7.5）。
                    report.unexpected_alive.push(sid.clone());
                    report.to_kill.push(sid.clone());
                }
                Some(_) => report.alive.push(sid.clone()),
                None => {
                    // server 无登记（重启后遗留）→ 意外存活，kill 清理。
                    report.unexpected_alive.push(sid.clone());
                    report.to_kill.push(sid.clone());
                }
            }
        }
        for sid in registered.keys() {
            if !alive.contains(sid) {
                report.missing.push(sid.clone());
            }
        }
        // pending_close 补发（§7.6：machine 重连后自动补发 kill）。
        for sid in &pending_close {
            if !report.to_kill.contains(sid) {
                report.to_kill.push(sid.clone());
            }
        }
        info!(
            machine_id,
            alive = report.alive.len(),
            unexpected_alive = report.unexpected_alive.len(),
            missing = report.missing.len(),
            to_kill = report.to_kill.len(),
            "alive_sessions reconciliation complete"
        );
        Ok(report)
    }

    /// pending_close 集合快照（诊断/测试）。
    pub async fn pending_close_sessions(&self) -> Vec<String> {
        self.inner
            .pending_close
            .read()
            .await
            .iter()
            .cloned()
            .collect()
    }

    /// binding 反向查询（hub session_id → acp_session_id；diagnostics）。
    pub async fn acp_session_id(&self, session_id: &str) -> Option<String> {
        self.inner
            .sessions
            .read()
            .await
            .get(session_id)
            .and_then(|e| e.acp_session_id.clone())
    }

    /// 活动 turn 登记（coordinator prompt 执行登记；§7.1 断链清理输入）。
    pub async fn set_active_turn(&self, session_id: &str, turn_id: &str) {
        self.inner
            .active_turns
            .write()
            .await
            .insert(session_id.to_string(), turn_id.to_string());
    }

    /// 活动 turn 清除（turn 终态 / 会话关闭 / 断链清理后）。
    pub async fn clear_active_turn(&self, session_id: &str) {
        self.inner.active_turns.write().await.remove(session_id);
    }

    /// 活动 turn 查询（断链清理：`MarkTurnInterrupted` 输入，§7.1）。
    pub async fn active_turn(&self, session_id: &str) -> Option<String> {
        self.inner.active_turns.read().await.get(session_id).cloned()
    }
}

#[cfg(test)]
#[path = "session_registry_test.rs"]
mod session_registry_test;
