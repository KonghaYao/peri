use std::collections::HashSet;
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
        let snapshot = resources
            .load_session_snapshot(parent_id)
            .await
            .map_err(|error| error.to_string())?;
        let BindingState::Bound(binding) = snapshot.binding else {
            return Err("Blocked: workflow parent execution binding unavailable".into());
        };
        let FrozenState::Present(frozen) = snapshot.frozen else {
            return Err("Blocked: workflow parent frozen authorization unavailable".into());
        };
        if snapshot.meta.cwd != context.cwd {
            return Err("Blocked: workflow execution workspace differs from parent".into());
        }
        let mut root_id = parent_id.clone();
        let mut seen = HashSet::new();
        loop {
            if seen.len() >= 128 || !seen.insert(root_id.clone()) {
                return Err("Blocked: workflow parent session ancestry is cyclic".into());
            }
            let ancestor = resources
                .load_session_snapshot(&root_id)
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
