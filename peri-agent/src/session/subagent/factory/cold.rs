use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use peri_acp_types::session_resources::work::{WorkAdmission, WorkQuery};
use peri_acp_types::session_resources::{ControlStatus, FrozenState, SessionResources};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::{SubagentChainAssembler, SubagentHost, SubagentLlmSource};
use super::context::build_subagent_session_v2;
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
        let revision = resources
            .load_work_revision(&metadata.child_session_id)
            .await?;
        let mut command = WorkCommand {
            session_id: metadata.child_session_id.clone(),
            recipient_lifecycle: metadata.recipient_lifecycle,
            mutation_id: "child-resume".into(),
            action: WorkAction::BindChildResumeMetadata {
                expected_revision: revision,
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
    let revision = resources
        .load_work_revision(&metadata.child_session_id)
        .await?;
    let mut command = WorkCommand {
        session_id: metadata.child_session_id.clone(),
        recipient_lifecycle: metadata.recipient_lifecycle,
        mutation_id: "child-owners".into(),
        action: WorkAction::BindResourceOwners {
            expected_revision: revision,
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
        TaskBinding, WorkAction, WorkCommand, WorkDecision, WorkResolution,
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
            binding,
        },
    };
    command.mutation_id = format!("child-delegation:{}", command.digest()?);
    let receipt = match resources.apply_work_mutation(&command).await {
        Ok(receipt) => receipt,
        Err(error)
            if error.effect() == peri_acp_types::session_resources::MutationOutcome::Unknown =>
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
        WorkDecision::Accepted => Ok(receipt),
        WorkDecision::Rejected { reason } => {
            tracing::warn!(
                initiator_session_id = initiator,
                invocation_id = invocation.intent.invocation_id,
                rejection = ?reason,
                "delegation task binding explicitly rejected"
            );
            Err(Box::new(crate::tools::EffectiveToolError::new(
                crate::tools::EffectiveToolErrorCode::ApplicationFailed,
                format!("delegation task binding rejected: {reason:?}"),
            )))
        }
    }
}

/// 子会话恢复 metadata（cold / live resume 共用，版本化）。
///
/// 版本语义（M3）：
/// - **v1**（历史记录）：`persona` 是创建时写入 transcript 的子身份 System
///   消息字节（durable 生产路径恒为 Some）；`system_prompt` 是创建时的父冻结
///   system 字节，**不是**子身份——恢复不得把它当身份（否则父能力声明进入
///   子请求面）。读取方按内容等价归一化，未知身份拒绝执行恢复。
/// - **v2**：`identity_system` 是子身份投影的确定字节（与 transcript 身份
///   注入已解耦，身份只经 bridge 注入）；`runtime_env` 为子冻结运行环境快照
///   （随父 frozen 继承，恢复不重探宿主）。
///
/// 版本字段不双写：v2 不再用 `persona` 承载身份，`persona` 只为 v1 解释保留。
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
    /// v1 身份载体（子身份 System 消息字节）；v2 恒 None（身份在
    /// `identity_system`，不双写）。
    pub persona: Option<String>,
    /// 创建时的父冻结 system 字节（审计/解释用；**不是**子身份）。
    pub system_prompt: String,
    /// v2：子身份投影字节（子能力投影后的确定身份）。
    #[serde(default)]
    pub identity_system: Option<String>,
    /// v2：子冻结运行环境快照（随父 frozen 继承；None = unavailable）。
    #[serde(default)]
    pub runtime_env: Option<peri_acp_types::frozen::FrozenRuntimeEnv>,
    pub claude_md: String,
    pub claude_local_md: Option<String>,
    pub skill_summary: String,
    pub date: String,
    pub language: Option<String>,
    pub section_overrides: BTreeMap<String, String>,
    pub disabled_middlewares: BTreeSet<String>,
    pub built_in_subagents_enabled: bool,
}

impl ChildResumeMetadata {
    /// 当前构建可执行的 metadata 版本集合（v1 = 历史记录，v2 = 身份结构化）。
    pub const SUPPORTED_VERSIONS: [u32; 2] = [1, 2];

    pub fn is_supported_version(&self) -> bool {
        Self::SUPPORTED_VERSIONS.contains(&self.version)
    }

    /// 版本意识身份归一化（M3）：
    /// - v2：`identity_system`（非空白才有效；None/空 = 创建时无身份，按"无
    ///   身份"恢复而不是拒绝）。
    /// - v1：优先 `persona`；缺失时回退调用方从子 transcript **首条 own 载荷**
    ///   提取的旧身份（`legacy_transcript_identity`）——旧写入器把身份同时写进
    ///   transcript，`persona` 只是同一字节的镜像。`system_prompt` 是父冻结
    ///   字节，**不参与**身份判定（不得把父能力声明当子身份）。
    /// - 未知版本：None（调用方拒绝执行恢复；历史读取不受影响）。
    pub fn resolved_identity<'a>(
        &'a self,
        legacy_transcript_identity: Option<&'a str>,
    ) -> Option<&'a str> {
        let non_blank = |value: &str| !value.trim().is_empty();
        match self.version {
            2 => self
                .identity_system
                .as_deref()
                .filter(|value| non_blank(value)),
            1 => self
                .persona
                .as_deref()
                .filter(|value| non_blank(value))
                .or_else(|| legacy_transcript_identity.filter(|value| non_blank(value))),
            _ => None,
        }
    }
}

/// 从子 own transcript 载荷提取旧身份（v1 归一化输入）。
///
/// 规则：**首条 own 载荷**且为 System 时才视为旧身份——旧 spawn 的身份注入
/// 发生在会话起始、parent_messages 之前；其它位置的 System（命令反馈 / 预测
/// 指令等）不参与，不做泛化过滤。
pub fn legacy_transcript_identity(
    payloads: &[peri_acp_types::store::PersistedPayload],
) -> Option<String> {
    match payloads.first()?.as_message()? {
        crate::messages::BaseMessage::System { content, .. } => Some(content.text_content()),
        _ => None,
    }
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
    /// 子模型来源（H1）：bridge（身份 + 请求时贡献）由 session factory 装配。
    pub llm: SubagentLlmSource,
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
        // M3：版本意识身份归一化——v2 用 identity_system，v1 优先 persona、
        // 回退子 transcript 首条 own System（旧写入器的身份载体）。未知版本仅
        // 拒绝**执行恢复**（历史与只读投影不受影响）；不重新读取已变化的
        // agent 定义猜身份。创建时确实无身份（无 system_builder）的子会话按
        // “无身份”恢复，而不是把父冻结字节当身份。
        if !metadata.is_supported_version() {
            return Err(ColdChildBlocked::new(
                "child identity unavailable for this metadata version; execution restore refused",
            ));
        }
        let legacy_transcript_identity = legacy_transcript_identity(&snapshot.payloads);
        let identity_system = metadata
            .resolved_identity(legacy_transcript_identity.as_deref())
            .unwrap_or_default()
            .to_owned();
        // v1 身份曾持久在 transcript System 里：模型投影吸收与身份逐字相同的
        // 那一条（恰一条；v2 起身份只经 bridge 注入）。
        let normalize_persisted_identity =
            metadata.version == 1 && !identity_system.trim().is_empty();
        if metadata.child_session_id != admission.session_id
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
            // M3：子身份 = 归一化后的确定身份投影（v1 persona / v2 identity_system），
            // 不再是创建时的父冻结字节。
            system_prompt: Arc::from(identity_system.as_str()),
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
            // M3：v2 metadata 携带子冻结运行环境快照（随父 frozen 继承）——恢复
            // 只消费它；v1 无该字段 = unavailable，不重探本地值冒充历史环境（H3）。
            runtime_env: metadata.runtime_env.clone(),
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
            // M3：v1 记录的身份经旧 transcript System 消息持久化——模型投影
            // 定向吸收与身份逐字相同的那一条，身份只经 bridge 注入一次。
            normalize_persisted_identity,
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

#[cfg(test)]
#[path = "cold_binding_test.rs"]
mod cold_binding_tests;
