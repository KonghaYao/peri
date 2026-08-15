//! Privacy-safe Peri Agent activity projection.
//!
//! This module is the wire privacy boundary for `peri.agentActivity`. It maps
//! canonical protocol-carrier events to a compact DTO without ever cloning raw
//! messages, prompts, reasoning, tool I/O, summaries, paths, errors or URLs.

use std::collections::BTreeMap;

use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::tasks::BgRegistryEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AGENT_ACTIVITY_SCHEMA_VERSION: u32 = 1;
const LABEL_MAX_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    Subagent,
    BackgroundTask,
    Compact,
    Context,
    LlmRetry,
    Workflow,
    Rewind,
    Diagnostics,
    Turn,
    Agent,
    System,
    Oauth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityStatus {
    Running,
    Completed,
    Failed,
    Warning,
    Suspended,
    Cancelled,
    Info,
}

/// Stable compact wire DTO. Every free-form source field must be discarded or
/// normalized before construction; maps contain mapper-owned keys only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivityWire {
    pub schema_version: u32,
    pub kind: AgentActivityKind,
    pub status: AgentActivityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_background: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl AgentActivityWire {
    fn new(kind: AgentActivityKind, status: AgentActivityStatus) -> Self {
        Self {
            schema_version: AGENT_ACTIVITY_SCHEMA_VERSION,
            kind,
            status,
            correlation_id: None,
            label: None,
            is_background: None,
            metrics: BTreeMap::new(),
            attributes: BTreeMap::new(),
        }
    }

    fn correlated(mut self, namespace: &str, value: &str) -> Self {
        self.correlation_id = Some(hash_correlation(namespace, value));
        self
    }

    fn labelled(mut self, value: &str) -> Self {
        self.label = safe_label(value);
        self
    }
}

/// Maps only user-relevant lifecycle facts. The exhaustive outer event match is
/// deliberate: a new event cannot silently inherit activity-wire eligibility.
pub fn map_agent_activity(event: &ExecutorEvent) -> Option<AgentActivityWire> {
    use AgentActivityKind as K;
    use AgentActivityStatus as S;

    let activity = match event {
        ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => {
            let mut item = AgentActivityWire::new(K::Subagent, S::Running)
                .correlated("subagent", instance_id)
                .labelled(agent_name);
            item.is_background = Some(*is_background);
            item
        }
        ExecutorEvent::SubagentStopped {
            agent_name,
            is_error,
            instance_id,
            result: _,
        } => AgentActivityWire::new(
            K::Subagent,
            if *is_error { S::Failed } else { S::Completed },
        )
        .correlated("subagent", instance_id)
        .labelled(agent_name),
        ExecutorEvent::BackgroundTaskCompleted(result) => {
            let mut item = AgentActivityWire::new(
                K::BackgroundTask,
                if result.success {
                    S::Completed
                } else {
                    S::Failed
                },
            )
            .correlated("background_task", &result.task_id)
            .labelled(&result.agent_name);
            item.is_background = Some(true);
            item.metrics
                .insert("tool_count".into(), result.tool_calls_count as u64);
            item.metrics
                .insert("duration_ms".into(), result.duration_ms);
            if result.timed_out {
                item.attributes.insert("reason".into(), "timed_out".into());
            }
            item
        }
        ExecutorEvent::CompactStarted {
            step,
            strategy,
            trigger,
            ..
        } => {
            let mut item =
                AgentActivityWire::new(K::Compact, S::Running).correlated("compact", "current");
            item.metrics.insert("step".into(), *step as u64);
            item.attributes
                .insert("strategy".into(), wire_name(strategy));
            item.attributes.insert("trigger".into(), wire_name(trigger));
            item
        }
        ExecutorEvent::CompactCompleted {
            files,
            skills,
            micro_cleared,
            token_before,
            token_after,
            strategy,
            trigger,
            outcome,
            affected_count,
            estimated_tokens_saved,
            ..
        } => {
            let outcome_name = wire_name(outcome);
            let failed = outcome_name.contains("failed") || outcome_name == "interrupted";
            let mut item =
                AgentActivityWire::new(K::Compact, if failed { S::Failed } else { S::Completed })
                    .correlated("compact", "current");
            item.metrics.insert("file_count".into(), files.len() as u64);
            item.metrics
                .insert("skill_count".into(), skills.len() as u64);
            item.metrics
                .insert("micro_cleared".into(), *micro_cleared as u64);
            item.metrics.insert("token_before".into(), *token_before);
            item.metrics.insert("token_after".into(), *token_after);
            item.metrics
                .insert("affected_count".into(), *affected_count as u64);
            item.metrics
                .insert("estimated_tokens_saved".into(), *estimated_tokens_saved);
            item.attributes
                .insert("strategy".into(), wire_name(strategy));
            item.attributes.insert("trigger".into(), wire_name(trigger));
            item.attributes.insert("outcome".into(), outcome_name);
            item
        }
        ExecutorEvent::CompactError { message: _ } => {
            AgentActivityWire::new(K::Compact, S::Failed).correlated("compact", "current")
        }
        ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            ..
        } => {
            let mut item =
                AgentActivityWire::new(K::Context, S::Warning).correlated("context", "current");
            item.metrics.insert("used_tokens".into(), *used_tokens);
            item.metrics.insert("total_tokens".into(), *total_tokens);
            item
        }
        ExecutorEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error: _,
        } => {
            let mut item = AgentActivityWire::new(K::LlmRetry, S::Warning);
            item.metrics.insert("attempt".into(), *attempt as u64);
            item.metrics
                .insert("max_attempts".into(), *max_attempts as u64);
            item.metrics.insert("delay_ms".into(), *delay_ms);
            item
        }
        ExecutorEvent::WorkflowProgress(progress) => {
            let status = match progress.run_status.as_deref() {
                Some("completed") => S::Completed,
                Some("failed" | "killed") => S::Failed,
                _ => match progress.agent_status.as_deref() {
                    Some("done") => S::Completed,
                    Some("dead") => S::Failed,
                    Some("skipped") => S::Cancelled,
                    _ => S::Running,
                },
            };
            let mut item = AgentActivityWire::new(K::Workflow, status)
                .correlated("workflow", &progress.run_id)
                .labelled(&progress.workflow_name);
            if let Some(value) = progress.token_count {
                item.metrics.insert("token_count".into(), value);
            }
            if let Some(value) = progress.tool_count {
                item.metrics.insert("tool_count".into(), value);
            }
            if let Some(value) = allowlisted(
                progress.event_type.as_str(),
                &[
                    "run_started",
                    "phase_started",
                    "phase_done",
                    "agent_started",
                    "agent_progress",
                    "agent_done",
                    "run_done",
                ],
            ) {
                item.attributes.insert("event_type".into(), value.into());
            }
            item
        }
        ExecutorEvent::RewindCompleted { .. } => AgentActivityWire::new(K::Rewind, S::Completed),
        ExecutorEvent::RewindError { message: _ } => AgentActivityWire::new(K::Rewind, S::Failed),
        ExecutorEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        } => {
            let status = if *errors > 0 {
                S::Failed
            } else if *warnings > 0 {
                S::Warning
            } else {
                S::Completed
            };
            let mut item =
                AgentActivityWire::new(K::Diagnostics, status).correlated("diagnostics", "current");
            item.metrics.insert("error_count".into(), *errors as u64);
            item.metrics
                .insert("warning_count".into(), *warnings as u64);
            item.metrics
                .insert("files_with_errors".into(), *files_with_errors as u64);
            item
        }
        ExecutorEvent::TurnSuspended { turn_id, .. } => {
            AgentActivityWire::new(K::Turn, S::Suspended).correlated("turn", turn_id)
        }
        ExecutorEvent::AgentExecutionFailed { message: _ } => {
            AgentActivityWire::new(K::Agent, S::Failed)
        }
        ExecutorEvent::SystemNotification { level, text: _ } => {
            let status = match level.as_str() {
                "error" => S::Failed,
                "warn" | "warning" => S::Warning,
                _ => S::Info,
            };
            AgentActivityWire::new(K::System, status)
        }
        ExecutorEvent::OauthNeeded {
            server_name,
            auth_url: _,
        } => AgentActivityWire::new(K::Oauth, S::Warning).labelled(server_name),
        ExecutorEvent::OauthCompleted { server_name } => {
            AgentActivityWire::new(K::Oauth, S::Completed).labelled(server_name)
        }
        ExecutorEvent::OauthFailed {
            server_name,
            error: _,
        } => AgentActivityWire::new(K::Oauth, S::Failed).labelled(server_name),
        ExecutorEvent::BudgetThresholdHit {
            threshold,
            tokens_in,
            tokens_out,
            ..
        } => {
            let mut item =
                AgentActivityWire::new(K::Context, S::Warning).correlated("context", "current");
            item.metrics.insert("tokens_in".into(), *tokens_in);
            item.metrics.insert("tokens_out".into(), *tokens_out);
            item.attributes
                .insert("threshold".into(), wire_name(threshold));
            item
        }
        ExecutorEvent::WorkflowStarted {
            workflow_id,
            plan_summary: _,
            ..
        } => AgentActivityWire::new(K::Workflow, S::Running).correlated("workflow", workflow_id),
        ExecutorEvent::WorkflowEnded {
            workflow_id,
            agents_spawned,
            tool_calls,
            ..
        } => {
            let mut item = AgentActivityWire::new(K::Workflow, S::Completed)
                .correlated("workflow", workflow_id);
            item.metrics
                .insert("agent_count".into(), *agents_spawned as u64);
            item.metrics.insert("tool_count".into(), *tool_calls as u64);
            item
        }
        ExecutorEvent::BgRegistryEvent(event) => map_bg_registry_activity(event),
        ExecutorEvent::AiReasoning { .. }
        | ExecutorEvent::TextChunk { .. }
        | ExecutorEvent::ToolStart { .. }
        | ExecutorEvent::ToolEnd { .. }
        | ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::TurnCommitted { .. }
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::MessageAdded(_)
        | ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmRequestPayload { .. }
        | ExecutorEvent::LlmCallEnd { .. }
        | ExecutorEvent::TodoUpdate(_)
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::SessionStarted { .. }
        | ExecutorEvent::TurnStarted { .. }
        | ExecutorEvent::TurnEnded { .. }
        | ExecutorEvent::MiddlewareStarted { .. }
        | ExecutorEvent::MiddlewareEnded { .. } => return None,
    };
    Some(activity)
}

fn map_bg_registry_activity(event: &BgRegistryEvent) -> AgentActivityWire {
    use AgentActivityKind::BackgroundTask;
    use AgentActivityStatus::{Cancelled, Completed, Failed, Running};
    match event {
        BgRegistryEvent::Started { task_id, kind, .. } => {
            let mut item = AgentActivityWire::new(BackgroundTask, Running)
                .correlated("background_task", task_id);
            item.is_background = Some(true);
            item.attributes.insert("task_kind".into(), wire_name(kind));
            item
        }
        BgRegistryEvent::Completed {
            task_id,
            kind,
            success,
            duration_ms,
            ..
        } => {
            let mut item =
                AgentActivityWire::new(BackgroundTask, if *success { Completed } else { Failed })
                    .correlated("background_task", task_id);
            item.is_background = Some(true);
            item.metrics.insert("duration_ms".into(), *duration_ms);
            if let Some(kind) = kind {
                item.attributes.insert("task_kind".into(), wire_name(kind));
            }
            item
        }
        BgRegistryEvent::Cancelled { task_id, reason: _ } => {
            let mut item = AgentActivityWire::new(BackgroundTask, Cancelled)
                .correlated("background_task", task_id);
            item.is_background = Some(true);
            item
        }
    }
}

fn safe_label(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(truncate_utf8(&normalized, LABEL_MAX_BYTES))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn hash_correlation(namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "unknown".into())
        .trim_matches('"')
        .to_string()
}

fn allowlisted<'a>(value: &'a str, allowed: &[&str]) -> Option<&'a str> {
    allowed.contains(&value).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::event::{BackgroundTaskResult, CompactStrategy, CompactTrigger};

    #[test]
    fn subagent_lifecycle_correlates_without_raw_identity_or_result() {
        let started = map_agent_activity(&ExecutorEvent::SubagentStarted {
            agent_name: " Research\nAgent ".into(),
            instance_id: "private-instance-id".into(),
            is_background: true,
        })
        .unwrap();
        let stopped = map_agent_activity(&ExecutorEvent::SubagentStopped {
            agent_name: "Research Agent".into(),
            result: "SECRET_RESULT_SENTINEL".into(),
            is_error: false,
            instance_id: "private-instance-id".into(),
        })
        .unwrap();
        assert_eq!(started.correlation_id, stopped.correlation_id);
        assert_eq!(started.label.as_deref(), Some("Research Agent"));
        let wire = serde_json::to_string(&stopped).unwrap();
        assert!(!wire.contains("private-instance-id"));
        assert!(!wire.contains("SECRET_RESULT_SENTINEL"));
    }

    #[test]
    fn compact_projection_contains_counts_not_sensitive_bodies() {
        let event = ExecutorEvent::CompactCompleted {
            summary: "SECRET_SUMMARY".into(),
            files: vec![peri_acp_types::event::CompactFileInfo {
                path: "/secret/path".into(),
                lines: 42,
            }],
            skills: vec!["private-skill".into()],
            micro_cleared: 3,
            messages: vec![],
            token_before: 1000,
            token_after: 600,
            strategy: CompactStrategy::Smart,
            affected_count: 4,
            estimated_tokens_saved: 400,
            estimated_tokens_before: 1000,
            estimated_tokens_after: 600,
            changed_messages: 2,
            changed_fields: 2,
            no_op_candidates: 0,
            full_escalation_reason: None,
            cache_hit_rate_before: 0.5,
            trigger: CompactTrigger::Auto,
            outcome: peri_acp_types::compact::CompactOutcome::SmartApplied,
        };
        let completed = map_agent_activity(&event).unwrap();
        let started = map_agent_activity(&ExecutorEvent::CompactStarted {
            turn_id: "raw-turn".into(),
            agent_id: "raw-agent".into(),
            step: 2,
            strategy: CompactStrategy::Smart,
            trigger: CompactTrigger::Auto,
        })
        .unwrap();
        assert_eq!(started.correlation_id, completed.correlation_id);
        let wire = serde_json::to_string(&completed).unwrap();
        assert!(wire.contains("\"file_count\":1"));
        assert!(wire.contains("\"skill_count\":1"));
        for prohibited in [
            "SECRET_SUMMARY",
            "/secret/path",
            "private-skill",
            "messages",
        ] {
            assert!(
                !wire.contains(prohibited),
                "prohibited field leaked: {prohibited}"
            );
        }
    }

    #[test]
    fn background_projection_omits_prompt_output_and_thread_identity() {
        let event = ExecutorEvent::BackgroundTaskCompleted(BackgroundTaskResult {
            task_id: "raw-task-id".into(),
            agent_name: "explorer".into(),
            prompt_summary: "SECRET_PROMPT".into(),
            success: false,
            output: "SECRET_OUTPUT".into(),
            tool_calls_count: 7,
            duration_ms: 900,
            timed_out: true,
            child_thread_id: Some("raw-thread-id".into()),
        });
        let wire = serde_json::to_string(&map_agent_activity(&event).unwrap()).unwrap();
        assert!(wire.contains("\"tool_count\":7"));
        assert!(wire.contains("timed_out"));
        for prohibited in [
            "raw-task-id",
            "SECRET_PROMPT",
            "SECRET_OUTPUT",
            "raw-thread-id",
        ] {
            assert!(!wire.contains(prohibited));
        }
    }
}
