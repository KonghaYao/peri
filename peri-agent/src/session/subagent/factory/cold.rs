use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use peri_acp_types::session_resources::work::{WorkAdmission, WorkQuery};
use peri_acp_types::session_resources::{ControlStatus, FrozenState, SessionResources};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::{SubagentChainAssembler, SubagentHost};
use super::context::build_subagent_session_v2;
use crate::agent::react::ReactLLM;
use crate::session::{FrozenContext, Session};
use crate::tools::{BaseTool, ToolInvocationResolver};

pub(super) async fn persist_child_resume_metadata(
    resources: &dyn SessionResources,
    metadata: &ChildResumeMetadata,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use peri_acp_types::session_resources::work::{
        WorkAction, WorkCommand, WorkDecision, WorkRejection, WorkResolution,
    };
    let serialized = serde_json::to_string(metadata)?;
    for _ in 0..3 {
        let snapshot = resources
            .load_session_work(&WorkQuery {
                session_id: metadata.child_session_id.clone(),
                limit: 1,
            })
            .await?;
        let mut command = WorkCommand {
            session_id: metadata.child_session_id.clone(),
            recipient_lifecycle: metadata.recipient_lifecycle,
            mutation_id: "child-resume".into(),
            action: WorkAction::BindChildResumeMetadata {
                expected_revision: snapshot.state.revision,
                metadata_json: serialized.clone(),
            },
        };
        command.mutation_id = format!("child-resume:{}", command.digest()?);
        let receipt = match resources.apply_work_mutation(&command).await {
            Ok(receipt) => receipt,
            Err(error)
                if error.effect()
                    == peri_acp_types::session_resources::MutationOutcome::Unknown =>
            {
                match resources.resolve_work_mutation(&command).await? {
                    WorkResolution::Applied { receipt } => receipt,
                    _ => return Err("Incomplete: child resume metadata ACK unknown".into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
            return Err("Incomplete: child resume metadata receipt conflict".into());
        }
        match receipt.decision {
            WorkDecision::Accepted => return Ok(()),
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision,
            } => continue,
            WorkDecision::Rejected { reason } => {
                return Err(format!("child resume metadata rejected: {reason:?}").into())
            }
        }
    }
    Err("Incomplete: child resume metadata revision did not settle".into())
}

pub(super) async fn copy_child_resource_owners(
    resources: &dyn SessionResources,
    metadata: &ChildResumeMetadata,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use peri_acp_types::session_resources::work::{
        WorkAction, WorkCommand, WorkDecision, WorkResolution,
    };
    let parent = resources
        .load_session_work(&WorkQuery {
            session_id: metadata.direct_initiator_session_id.clone(),
            limit: 1,
        })
        .await?;
    let owners = parent
        .state
        .resource_owners
        .get(&metadata.direct_initiator_lifecycle)
        .ok_or("Blocked: original child owner authorization declarations unavailable")?;
    let child = resources
        .load_session_work(&WorkQuery {
            session_id: metadata.child_session_id.clone(),
            limit: 1,
        })
        .await?;
    let mut command = WorkCommand {
        session_id: metadata.child_session_id.clone(),
        recipient_lifecycle: metadata.recipient_lifecycle,
        mutation_id: "child-owners".into(),
        action: WorkAction::BindResourceOwners {
            expected_revision: child.state.revision,
            connections_json: owners.connections_json.clone(),
            authorization_ref: owners.authorization_ref.clone(),
        },
    };
    command.mutation_id = format!("child-owners:{}", command.digest()?);
    let receipt = match resources.apply_work_mutation(&command).await {
        Ok(receipt) => receipt,
        Err(error)
            if error.effect() == peri_acp_types::session_resources::MutationOutcome::Unknown =>
        {
            match resources.resolve_work_mutation(&command).await? {
                WorkResolution::Applied { receipt } => receipt,
                _ => return Err("Incomplete: child owner declaration ACK unknown".into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    if receipt.session_id != command.session_id
        || receipt.mutation_id != command.mutation_id
        || receipt.decision != WorkDecision::Accepted
    {
        return Err("Incomplete: child owner declaration receipt unavailable".into());
    }
    Ok(())
}

pub(super) async fn bind_delegation_task(
    resources: &dyn SessionResources,
    initiator: &str,
    invocation: &peri_acp_types::session_resources::work::InvocationRecord,
    task_id: &str,
) -> Result<
    peri_acp_types::session_resources::work::WorkReceipt,
    Box<dyn std::error::Error + Send + Sync>,
> {
    use peri_acp_types::session_resources::work::{
        TaskBinding, WorkAction, WorkCommand, WorkDecision, WorkRejection, WorkResolution,
    };
    let binding = TaskBinding {
        invocation_id: invocation.intent.invocation_id.clone(),
        owner_identity: invocation.intent.owner_identity.clone(),
        owner_task_id: task_id.into(),
        initiator_session_id: initiator.into(),
        recipient_lifecycle: invocation.recipient_lifecycle,
        recovery_locator: invocation.intent.recovery_locator.clone(),
        authorization_ref: invocation.intent.authorization_ref.clone(),
    };
    for _ in 0..3 {
        let work = resources
            .load_session_work(&WorkQuery {
                session_id: initiator.into(),
                limit: 1,
            })
            .await?;
        let mut command = WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: invocation.recipient_lifecycle,
            mutation_id: "child-delegation".into(),
            action: WorkAction::ReconcileTaskBinding {
                expected_revision: work.state.revision,
                binding: binding.clone(),
            },
        };
        command.mutation_id = format!("child-delegation:{}", command.digest()?);
        let receipt = match resources.apply_work_mutation(&command).await {
            Ok(receipt) => receipt,
            Err(error)
                if error.effect()
                    == peri_acp_types::session_resources::MutationOutcome::Unknown =>
            {
                match resources.resolve_work_mutation(&command).await? {
                    WorkResolution::Applied { receipt } => receipt,
                    _ => return Err("Incomplete: delegation task binding ACK unknown".into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
            return Err("Incomplete: delegation binding receipt conflicts".into());
        }
        match receipt.decision {
            WorkDecision::Accepted => return Ok(receipt),
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision,
            } => continue,
            WorkDecision::Rejected { reason } => {
                return Err(format!("delegation task binding rejected: {reason:?}").into())
            }
        }
    }
    Err("Incomplete: delegation binding revision did not settle".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildResumeMetadata {
    pub version: u32,
    pub child_session_id: String,
    pub recipient_lifecycle: u64,
    pub agent_name: String,
    pub model_name: String,
    pub direct_initiator_session_id: String,
    pub direct_initiator_lifecycle: u64,
    pub delegation_invocation_id: String,
    pub delegation_task_id: String,
    pub authorization_ref: String,
    pub frozen_digest: String,
    pub tool_ceiling: BTreeSet<String>,
    pub tool_origins: BTreeMap<String, Option<String>>,
    pub skill_names: Vec<String>,
    pub max_iterations: usize,
    pub persona: Option<String>,
    pub system_prompt: String,
    pub claude_md: String,
    pub claude_local_md: Option<String>,
    pub skill_summary: String,
    pub date: String,
    pub language: Option<String>,
    pub section_overrides: BTreeMap<String, String>,
    pub disabled_middlewares: BTreeSet<String>,
    pub built_in_subagents_enabled: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("cold child execution blocked: {reason}")]
pub struct ColdChildBlocked {
    pub reason: String,
}

impl ColdChildBlocked {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

pub struct ColdChildRuntime {
    pub resources: Arc<dyn SessionResources>,
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    pub tools: Vec<Arc<dyn BaseTool>>,
    pub host: Arc<SubagentHost>,
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    pub authorized_ceiling: BTreeSet<String>,
    pub authorization_ref: String,
}

pub struct ColdChildExecution {
    pub session: Arc<Session>,
    pub context: crate::agent::stages::StageContext,
    pub event_handles: Option<crate::agent::events_v2::EventHandles>,
    pub metadata: ChildResumeMetadata,
    pub delegation_binding: peri_acp_types::session_resources::work::TaskBinding,
}

impl ColdChildExecution {
    pub async fn terminal_command(
        &self,
        result: &peri_acp_types::event::BackgroundTaskResult,
    ) -> Result<peri_acp_types::session_resources::work::WorkCommand, ColdChildBlocked> {
        let host = self
            .session
            .subagent_host()
            .ok_or_else(|| ColdChildBlocked::new("child resource host unavailable"))?;
        let resources = host
            .session_resources
            .as_ref()
            .ok_or_else(|| ColdChildBlocked::new("child resources unavailable"))?;
        let admission = self
            .context
            .session
            .turn
            .work_admission()
            .ok_or_else(|| ColdChildBlocked::new("original SDK admission unavailable"))?;
        let child = resources
            .load_session_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if result.task_id != self.delegation_binding.owner_task_id
            || child.state.work_delegations.get(&admission.work_id)
                != Some(&self.delegation_binding)
            || !child
                .state
                .admissions
                .get(&admission.admission_id)
                .is_some_and(|record| {
                    record.admission == *admission && record.entering_receipt.is_some()
                })
            || child.control.attempt.is_some()
        {
            return Err(ColdChildBlocked::new(
                "model stopped evidence or immutable delegation identity missing",
            ));
        }
        if child.control.status == ControlStatus::Closing {
            super::super::close_subagent_session_scope(self.session.clone())
                .await
                .map_err(|error| {
                    ColdChildBlocked::new(format!("child close incomplete: {error}"))
                })?;
        }
        let parent = resources
            .load_session_work(&WorkQuery {
                session_id: self.delegation_binding.initiator_session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        let binding = parent
            .state
            .task_bindings
            .get(&self.delegation_binding.invocation_id)
            .ok_or_else(|| {
                ColdChildBlocked::new("original immutable delegation binding unavailable")
            })?;
        if binding != &self.delegation_binding {
            return Err(ColdChildBlocked::new(
                "current work delegation binding conflicts",
            ));
        }
        let reminder = crate::session::async_router::background_result_reminder(
            result,
            peri_acp_types::tasks::BgTaskKind::Agent,
        );
        let delivery_id = crate::agent::async_tasks::delivery::terminal_delivery_id(
            &binding.owner_task_id,
            "terminal",
        );
        crate::agent::async_tasks::build_task_terminal_command(
            &binding.initiator_session_id,
            binding.recipient_lifecycle,
            binding,
            delivery_id,
            &reminder,
            peri_acp_types::session::MessageSource::SubAgentComplete,
        )
        .map_err(ColdChildBlocked::new)
    }
}

impl super::SessionFactory {
    pub async fn prepare_cold_child_execution(
        admission: WorkAdmission,
        runtime: ColdChildRuntime,
    ) -> Result<ColdChildExecution, ColdChildBlocked> {
        let availability = runtime
            .resources
            .inspect_availability(Some(&admission.session_id))
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if availability.access != peri_acp_types::session_resources::AccessMode::ReadWrite
            || availability.execution
                != Some(peri_acp_types::session_resources::ExecutionAvailability::Available)
        {
            return Err(ColdChildBlocked::new(
                "same-store execution authorization is unavailable",
            ));
        }
        let snapshot = runtime
            .resources
            .load_session_snapshot(&admission.session_id)
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if snapshot.meta.parent_thread_id.is_none() {
            return Err(ColdChildBlocked::new("target is not a child session"));
        }
        let work = runtime
            .resources
            .load_session_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if work.control.status != ControlStatus::Active
            || work.control.lifecycle != admission.lifecycle
            || work.control.control_generation != admission.control_generation
            || !work
                .state
                .admissions
                .get(&admission.admission_id)
                .is_some_and(|record| {
                    record.admission == admission
                        && record.entering_receipt.is_some()
                        && record.settled_receipt.is_none()
                })
        {
            return Err(ColdChildBlocked::new(
                "exact SDK admission is not durably active",
            ));
        }
        let raw = work
            .state
            .child_resume_metadata
            .get(&admission.lifecycle)
            .ok_or_else(|| {
                ColdChildBlocked::new("persisted child authorization metadata missing")
            })?;
        let metadata: ChildResumeMetadata = serde_json::from_str(raw)
            .map_err(|error| ColdChildBlocked::new(format!("invalid child metadata: {error}")))?;
        let delegation_binding = work
            .state
            .work_delegations
            .get(&admission.work_id)
            .cloned()
            .ok_or_else(|| {
                ColdChildBlocked::new("current immutable work delegation reference missing")
            })?;
        let FrozenState::Present(bytes) = snapshot.frozen else {
            return Err(ColdChildBlocked::new("persisted frozen snapshot missing"));
        };
        if metadata.version != 1
            || metadata.child_session_id != admission.session_id
            || metadata.recipient_lifecycle != admission.lifecycle
            || snapshot.meta.parent_thread_id.as_deref()
                != Some(metadata.direct_initiator_session_id.as_str())
            || metadata.direct_initiator_lifecycle == 0
            || metadata.delegation_invocation_id.is_empty()
            || metadata.delegation_task_id.is_empty()
            || metadata.authorization_ref.is_empty()
            || metadata.authorization_ref != runtime.authorization_ref
            || metadata.model_name == "unknown"
            || runtime.llm.model_name() != metadata.model_name
            || metadata.frozen_digest != format!("{:x}", Sha256::digest(bytes.as_str().as_bytes()))
            || !metadata.tool_ceiling.is_subset(&runtime.authorized_ceiling)
            || runtime.host.execution_admission_port.is_none()
        {
            return Err(ColdChildBlocked::new(
                "child identity, frozen data or authorization ceiling unavailable",
            ));
        }
        let available: BTreeSet<_> = runtime
            .tools
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        if !metadata.tool_ceiling.is_subset(&available) {
            return Err(ColdChildBlocked::new(
                "saved child tool capability cannot be reconstructed",
            ));
        }
        if metadata
            .tool_origins
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != metadata.tool_ceiling
            || runtime
                .tools
                .iter()
                .filter(|tool| metadata.tool_ceiling.contains(tool.name()))
                .any(|tool| {
                    metadata
                        .tool_origins
                        .get(tool.name())
                        .map(|origin| origin.as_deref())
                        != Some(tool.mcp_server_name())
                })
        {
            return Err(ColdChildBlocked::new(
                "saved child tool origin cannot be reconstructed",
            ));
        }
        let parent_work = runtime
            .resources
            .load_session_work(&WorkQuery {
                session_id: delegation_binding.initiator_session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if !parent_work
            .state
            .invocations
            .get(&delegation_binding.invocation_id)
            .is_some_and(|record| {
                record.recipient_lifecycle == delegation_binding.recipient_lifecycle
                    && record.intent.authorization_ref == delegation_binding.authorization_ref
            })
        {
            return Err(ColdChildBlocked::new(
                "original delegation binding unavailable",
            ));
        }
        if parent_work
            .state
            .task_bindings
            .get(&delegation_binding.invocation_id)
            != Some(&delegation_binding)
        {
            return Err(ColdChildBlocked::new(
                "immutable original delegation task binding unavailable",
            ));
        }
        let ceiling = metadata.tool_ceiling.clone();
        let tool_filter: crate::session::tool_catalog::ToolFilter =
            Arc::new(move |tool| ceiling.contains(tool.name()));
        let frozen = FrozenContext {
            system_prompt: Arc::from(metadata.system_prompt.as_str()),
            claude_md: Arc::from(metadata.claude_md.as_str()),
            skill_summary: Arc::from(metadata.skill_summary.as_str()),
            date: Arc::from(metadata.date.as_str()),
            language: metadata.language.as_deref().map(Arc::from),
            meta_harness: peri_acp_types::meta_harness::MetaHarnessState {
                section_overrides: metadata
                    .section_overrides
                    .iter()
                    .map(|(key, value)| (key.clone(), Arc::from(value.as_str())))
                    .collect(),
                disabled_middlewares: metadata.disabled_middlewares.iter().cloned().collect(),
                built_in_subagents_enabled: metadata.built_in_subagents_enabled,
            },
        };
        let mut inherited = snapshot.inherited;
        inherited.flags.extend(snapshot.flags);
        let (session, context) = build_subagent_session_v2(
            snapshot.meta.cwd,
            frozen,
            Default::default(),
            admission.session_id.clone(),
            Some(runtime.host.clone()),
            Some(runtime.resources),
            inherited,
            snapshot.payloads,
            runtime.llm,
            runtime.chain_assembler,
            runtime.tools,
            tool_filter,
            runtime.host.session_mcp_capability.clone(),
            metadata.skill_names.clone(),
            Some(metadata.claude_md.clone()),
            metadata.claude_local_md.clone(),
            Some(metadata.skill_summary.clone()),
            runtime.tool_invocation_resolver,
            None,
            None,
            None,
            Some(super::super::agent_id_from_child_thread(
                &admission.session_id,
            )),
        )
        .await
        .map_err(|error| ColdChildBlocked::new(error.to_string()))?;
        if !context.context.session.turn.bind_work_admission(admission) {
            return Err(ColdChildBlocked::new(
                "execution already has a different SDK ticket",
            ));
        }
        Ok(ColdChildExecution {
            session,
            context: context.context,
            event_handles: Some(context.event_handles),
            metadata,
            delegation_binding,
        })
    }
}

#[cfg(test)]
#[path = "cold_test.rs"]
mod cold_tests;
