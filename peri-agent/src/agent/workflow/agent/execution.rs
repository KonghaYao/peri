use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, FrozenState, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::InheritedContext;
use peri_acp_types::thread::CancelPolicy;
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
