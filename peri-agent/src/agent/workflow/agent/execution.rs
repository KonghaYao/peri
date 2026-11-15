use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, FrozenState, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::InheritedContext;
use peri_acp_types::thread::{AgentStatus, CancelPolicy, ThreadId, WorkState};
use peri_acp_types::workflow::AgentRunParams;

use super::WorkflowAgentContext;

pub(super) struct WorkflowExecution {
    pub(super) session_id: String,
    pub(super) resources: Arc<dyn SessionResources>,
}

impl WorkflowExecution {
    pub(super) async fn prepare(
        context: &WorkflowAgentContext,
        params: &AgentRunParams,
    ) -> Result<Self, String> {
        let resources = context
            .session_resources
            .clone()
            .ok_or("Blocked: workflow session resources unavailable")?;
        let parent_id = context
            .session_id
            .as_ref()
            .ok_or("Blocked: workflow initiating session identity unavailable")?;
        // 准入只消费 meta/binding/frozen：读有效视图，不加载 own 正文。
        let snapshot = resources
            .load_active_snapshot(parent_id)
            .await
            .map_err(|error| error.to_string())?;
        let BindingState::Bound(binding) = snapshot.binding else {
            return Err("Blocked: workflow parent execution binding unavailable".into());
        };
        let FrozenState::Present(frozen) = snapshot.frozen else {
            return Err("Blocked: workflow parent frozen authorization unavailable".into());
        };
        ensure_same_execution_directory(&snapshot.meta.cwd, &context.cwd)?;
        let mut root_id = parent_id.clone();
        let mut seen = HashSet::new();
        loop {
            if seen.len() >= 128 || !seen.insert(root_id.clone()) {
                return Err("Blocked: workflow parent session ancestry is cyclic".into());
            }
            let ancestor = resources
                .load_active_snapshot(&root_id)
                .await
                .map_err(|error| error.to_string())?;
            match ancestor.meta.parent_thread_id {
                Some(parent) => root_id = parent,
                None => break,
            }
        }
        let session_id = uuid::Uuid::now_v7().to_string();
        resources
            .save_child(&ChildSnapshot {
                target: NewSession {
                    thread_id: session_id.clone(),
                    created_at: peri_time::now_utc_rfc3339(),
                    meta: NewSessionMeta {
                        title: Some(format!("workflow:{}:{}", params.run_id, params.agent_id)),
                        cwd: context.cwd.clone(),
                        parent_thread_id: Some(parent_id.clone()),
                        hidden: true,
                        cancel_policy: CancelPolicy::Cascade,
                        snapshot_at_message_id: None,
                    },
                    binding,
                    frozen,
                },
                parent_id: parent_id.clone(),
                root_id,
                inherited: InheritedContext::default(),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            session_id,
            resources,
        })
    }

    pub(super) fn settlement(
        &self,
        session: Arc<crate::session::Session>,
    ) -> WorkflowChildSettlement {
        WorkflowChildSettlement {
            resources: Arc::clone(&self.resources),
            thread_id: ThreadId::from(self.session_id.clone()),
            session,
            settled: false,
        }
    }
}

/// `execute` 任何退出路径上的 child 工作意图结算。
///
/// workflow child 以 `active + working` 入队初始指令；run 结束后若只关闭会话作用域，
/// 父会话每次进入都会把这个 child 当作待续跑候选。显式路径 await [`Self::settle`]，
/// 把终态与 `work_state` 交给资源侧一次提交；future 被 workflow kill / 会话关闭
/// drop 或 panic 时无法 await，由 [`Drop`] 先停掉本次 child 的后台工作再结算。
pub(super) struct WorkflowChildSettlement {
    resources: Arc<dyn SessionResources>,
    thread_id: ThreadId,
    session: Arc<crate::session::Session>,
    settled: bool,
}

impl WorkflowChildSettlement {
    pub(super) async fn settle(&mut self, status: AgentStatus) {
        settle_workflow_child(self.resources.as_ref(), &self.thread_id, status).await;
        // await 期间被 drop 时保持未结算，让 Drop 兜底；正常返回后不再追加写入。
        self.settled = true;
    }
}

impl Drop for WorkflowChildSettlement {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // 取消（kill / 调用方 drop）与 panic 都走不到显式返回；前者按取消收口，
        // 后者是执行失败。没有 runtime 时只能留下可诊断日志，不伪装已结算。
        let status = if std::thread::panicking() {
            AgentStatus::Error
        } else {
            AgentStatus::Cancelled
        };
        let resources = Arc::clone(&self.resources);
        let thread_id = self.thread_id.clone();
        let session = Arc::clone(&self.session);
        // 先同步停掉模型/工具工作，再在后台任务中等 close 收敛；终态必须等
        // 后台工作停下后再写，避免把「已结束」与仍在运行的任务同时暴露。
        session.config().cancel_token.cancel();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                session_id = self.thread_id.as_str(),
                ?status,
                "workflow child settlement skipped without an async runtime; child remains active"
            );
            return;
        };
        handle.spawn(async move {
            if let Err(error) =
                crate::session::subagent::close_subagent_session_scope(session).await
            {
                tracing::warn!(
                    session_id = thread_id.as_str(),
                    %error,
                    "workflow child scope close incomplete after cancelled execution"
                );
            }
            settle_workflow_child(resources.as_ref(), &thread_id, status).await;
        });
    }
}

async fn settle_workflow_child(
    resources: &dyn SessionResources,
    thread_id: &ThreadId,
    status: AgentStatus,
) {
    // 与 sync/background child 同一生命周期映射：取消保留停止意图，其余收尾回到
    // 不自动续跑；终态与 work_state 由资源侧一次提交。
    let work_state = WorkState::after_agent_terminal(status);
    if let Err(error) = resources
        .settle_terminal_without_result(thread_id, status, work_state)
        .await
    {
        // 结算失败不得吞掉：child 仍可能停在 working，必须留下可定位证据。
        tracing::error!(
            session_id = thread_id.as_str(),
            ?status,
            %work_state,
            %error,
            "workflow child terminal settlement failed"
        );
    }
}

/// 父快照与本次 workflow 执行目录必须指向同一目录。
///
/// 与 subagent spawn 同一判定（`same_execution_directory`）：绑定恢复可能带回
/// 尾分隔符，文本差异不构成换目录；真正不同的目录仍拒绝。
fn ensure_same_execution_directory(parent_cwd: &str, cwd: &str) -> Result<(), String> {
    if peri_acp_types::workspace::same_execution_directory(Path::new(parent_cwd), Path::new(cwd)) {
        Ok(())
    } else {
        Err("Blocked: workflow execution workspace differs from parent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_same_execution_directory;

    #[test]
    fn workflow_guard_accepts_same_directory_text_variants() {
        assert!(ensure_same_execution_directory("/a/workspace", "/a/workspace/").is_ok());
        assert!(ensure_same_execution_directory("/a/workspace/", "/a/workspace").is_ok());
        assert!(ensure_same_execution_directory("/a/workspace", "/a/./workspace").is_ok());
    }

    #[test]
    fn workflow_guard_rejects_different_directory() {
        for other in ["/a/workspace/sub", "/a/other", "/a/workspace/../other"] {
            assert_eq!(
                ensure_same_execution_directory("/a/workspace", other),
                Err("Blocked: workflow execution workspace differs from parent".into())
            );
        }
    }
}
