//! Bounded, monotone planning against the already committed model view.
use std::collections::{HashMap, HashSet};

use super::super::projection::{
    render_micro_plan, render_persisted_llm_view, MicroCompactPlan, ProjectionAction,
    ProjectionActionEntry, ProjectionTarget, ProviderCapabilities, ProviderProtocol,
    PROJECTION_POLICY_VERSION,
};
use super::{
    index::{Exchange, HistoryIndex},
    CompactConfig, ContextPressure, MicroPlanningFacts, PlanningDiagnostics,
};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageContent, MessageId};
use crate::session::transcript::MessageTranscript;
use crate::tools::ContextRetention;
use peri_acp_types::context_archive::{build_catalog, ArchiveRoot};

// Bounds per-message directive growth and whole-batch protocol work, including
// inline batches whose many exchanges share only two canonical message IDs.
const MAX_GROUP_EXCHANGES: usize = 32;

fn archive_error(error: peri_acp_types::context_archive::ArchiveError) -> AgentError {
    AgentError::Other(anyhow::Error::new(error))
}

fn policy(name: &str, config: &CompactConfig, facts: &MicroPlanningFacts) -> ContextRetention {
    let name = name.to_lowercase();
    if config
        .micro_excluded_tools
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(&name))
    {
        return ContextRetention::Preserve;
    }
    let declared = facts
        .tool_retention
        .get(&name)
        .copied()
        .unwrap_or(ContextRetention::PreviewOnly);
    // A configuration may add protection, never authorize erasing an unknown side effect.
    if matches!(
        declared,
        ContextRetention::Preserve | ContextRetention::StateBearing
    ) {
        return declared;
    }
    match config.tool_retention_map.get(&name) {
        Some(ContextRetention::Preserve) => ContextRetention::Preserve,
        Some(ContextRetention::StateBearing) => ContextRetention::StateBearing,
        _ => declared,
    }
}

fn tokens(message: &BaseMessage) -> u64 {
    crate::agent::token::estimate_request_tokens(std::slice::from_ref(message), &[])
}

pub(super) fn preview_chars(original: usize, head: usize, tail: usize) -> usize {
    if original <= head.saturating_add(tail) {
        return original;
    }
    let marker = peri_acp_types::sentinel::format_projection_sentinel_v1(
        original.saturating_sub(head + tail),
    );
    original.min(
        head.saturating_add(tail)
            .saturating_add(marker.chars().count() + 2),
    )
}

fn strong_projections(transcript: &MessageTranscript) -> (HashSet<&str>, HashSet<MessageId>) {
    let mut calls = HashSet::new();
    let mut messages = HashSet::new();
    for source in transcript.entries() {
        if let Some(directive) = transcript.projection_ref(source.id()) {
            for entry in &directive.entries {
                match &entry.action {
                    ProjectionAction::OmitToolExchange { tool_call_ids, .. } => {
                        calls.extend(tool_call_ids.iter().map(String::as_str));
                    }
                    ProjectionAction::ResultReceipt { .. }
                        if matches!(entry.target, ProjectionTarget::Message) =>
                    {
                        messages.insert(source.id());
                    }
                    _ => {}
                }
            }
        }
    }
    (calls, messages)
}

fn record_unhandled_content(message: &BaseMessage, diagnostics: &mut PlanningDiagnostics) {
    fn visit(block: &crate::messages::ContentBlock, diagnostics: &mut PlanningDiagnostics) {
        use crate::messages::ContentBlock;
        let (kind, estimate) = match block {
            ContentBlock::Image { .. } | ContentBlock::Document { .. } => {
                ("media", crate::agent::token::BINARY_MEDIA_ESTIMATED_TOKENS)
            }
            ContentBlock::Unknown(value) => {
                let estimate = (value.to_string().chars().count() as u64).div_ceil(4);
                diagnostics.unhandled("unknown_block", estimate);
                ("unknown", estimate)
            }
            ContentBlock::Reasoning { text, .. } => {
                ("reasoning", (text.chars().count() as u64).div_ceil(4))
            }
            ContentBlock::ToolResult { content, .. } => {
                for child in content {
                    visit(child, diagnostics);
                }
                return;
            }
            _ => return,
        };
        *diagnostics
            .remaining_tokens_by_kind
            .entry(kind.into())
            .or_default() += estimate;
    }
    match message.message_content() {
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                visit(block, diagnostics);
            }
        }
        MessageContent::Raw(values) => {
            for value in values {
                if !matches!(
                    value["type"].as_str(),
                    Some(
                        "text"
                            | "tool_use"
                            | "tool_result"
                            | "image"
                            | "document"
                            | "thinking"
                            | "reasoning"
                            | "redacted_thinking"
                    )
                ) {
                    diagnostics.unhandled(
                        "unknown_block",
                        (value.to_string().chars().count() as u64).div_ceil(4),
                    );
                }
            }
        }
        MessageContent::Text(_) => {}
    }
}

/// Plan once after the existing gate. No flags, persistence, tools or model calls occur here.
pub fn plan_micro_with_facts(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    pressure: &ContextPressure,
    facts: &MicroPlanningFacts,
    caps: &ProviderCapabilities,
) -> AgentResult<MicroCompactPlan> {
    #[cfg(test)]
    super::PLAN_MICRO_CALLS.with(|calls| calls.set(calls.get() + 1));
    let index = HistoryIndex::collect(transcript)?;
    let current = render_persisted_llm_view(transcript, caps)?;
    let current_by_id: HashMap<_, _> = current
        .iter()
        .map(|message| (message.id(), message))
        .collect();
    let current_costs: HashMap<_, _> = current
        .iter()
        .map(|message| (message.id(), tokens(message)))
        .collect();
    let (strong_calls, strong_messages) = strong_projections(transcript);
    let result_costs = super::costs::result_costs(&current);
    let input_costs: HashMap<_, _> = current
        .iter()
        .flat_map(|message| message.tool_calls())
        .map(|call| {
            (
                call.id.as_str(),
                ((call.name.chars().count() + call.arguments.to_string().chars().count()) as u64)
                    .div_ceil(4),
            )
        })
        .collect();
    let before = crate::agent::token::estimate_request_tokens(&current, &[])
        .saturating_add(facts.fixed_view_tokens);
    let desired = ((pressure.context_window as u64 * 45) / 100).min(pressure.target_tokens());
    let soft_budget = 8192.min(pressure.context_window as u64 / 10);
    let receipt_budget = 2048.min(pressure.context_window as u64 * 2 / 100);
    let mut diagnostics = PlanningDiagnostics {
        trigger_pressure_tokens: pressure.estimated_tokens,
        desired_view_tokens: desired,
        soft_recent_budget_tokens: soft_budget,
        receipt_budget_tokens: receipt_budget,
        scanned_messages: transcript.entries().len(),
        ..Default::default()
    };
    diagnostics.protect("frozen_system_and_tools", facts.fixed_view_tokens);
    let completed: Vec<usize> = index
        .steps
        .iter()
        .enumerate()
        .filter_map(|(si, step)| {
            step.exchanges
                .iter()
                .all(|ei| index.exchanges[*ei].result.is_some())
                .then_some(si)
        })
        .collect();
    let recent: HashSet<usize> = completed
        .iter()
        .rev()
        .take(config.micro_compact_stale_steps)
        .copied()
        .collect();
    let mut recent_tokens: u64 = index
        .steps
        .iter()
        .enumerate()
        .filter(|(si, _)| recent.contains(si))
        .map(|(_, step)| {
            let mut ids = HashSet::from([step.entry.id()]);
            for ei in &step.exchanges {
                if let Some(result) = &index.exchanges[*ei].result {
                    ids.insert(result.entry.id());
                }
            }
            ids.into_iter()
                .filter_map(|id| current_costs.get(&id))
                .sum::<u64>()
        })
        .sum();
    let mut plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: pressure.target_reclaim_tokens(),
        estimated_before_tokens: before,
        estimated_after_tokens: before,
        ..Default::default()
    };
    let mut eligible = HashSet::new();
    let mut preview_catalogs = super::catalogs::PreviewCatalogs::default();
    let mut preview_counts: HashMap<MessageId, usize> = HashMap::new();
    let mut projected_savings = HashMap::new();
    for (ei, exchange) in index.exchanges.iter().enumerate() {
        let step = &index.steps[exchange.step];
        let Some(result) = &exchange.result else {
            diagnostics.protect(
                "unresolved_call",
                current_costs.get(&step.entry.id()).copied().unwrap_or(0),
            );
            continue;
        };
        let result_tokens = result_costs.get(exchange.call_id).map_or(0, |cost| cost.1);
        let content_kind = if result.is_error {
            "error"
        } else {
            "tool_result"
        };
        *diagnostics
            .remaining_tokens_by_kind
            .entry(content_kind.into())
            .or_default() += result_tokens;
        if facts.protected_message_ids.contains(&result.entry.id())
            || facts.protected_message_ids.contains(&step.entry.id())
        {
            diagnostics.protect("protected_result", result_tokens);
            continue;
        }
        if strong_calls.contains(exchange.call_id)
            || strong_messages.contains(&result.entry.id())
            || strong_messages.contains(&step.entry.id())
        {
            continue;
        }
        if !facts.exposed_message_ids.contains(&result.entry.id()) {
            diagnostics.protect("not_exposed", result_tokens);
            continue;
        }
        if let Some(BaseMessage::Tool {
            execution: Some(execution),
            ..
        }) = result.entry.as_message()
        {
            if matches!(
                execution.status,
                crate::tools::ToolExecutionStatus::Unknown
                    | crate::tools::ToolExecutionStatus::Running
                    | crate::tools::ToolExecutionStatus::RunningAfterTimeout
            ) || (execution.task_id.is_some()
                && !facts.closed_call_ids.contains(exchange.call_id))
            {
                diagnostics.protect("unsettled_execution", result_tokens);
                continue;
            }
        }
        let retention = policy(exchange.tool_name, config, facts);
        if matches!(
            retention,
            ContextRetention::Preserve | ContextRetention::StateBearing
        ) {
            diagnostics.protect(
                if retention == ContextRetention::Preserve {
                    "explicit_semantic_protection"
                } else {
                    "effective_state_without_replacement"
                },
                result_tokens,
            );
            continue;
        }
        if retention == ContextRetention::ArchiveRead {
            if !matches!(result.target, ProjectionTarget::Message) {
                diagnostics.unhandled("protocol_dependency", result_tokens);
                continue;
            }
            let source = serde_json::from_value::<
                peri_acp_types::context_archive::ArchiveReadRequest,
            >(exchange.arguments.clone());
            let Ok(source) = source else {
                diagnostics.unhandled("recovery_chain_protected", result_tokens);
                continue;
            };
            if peri_acp_types::context_archive::ArchiveReference::decode(&source.archive_ref)
                .is_err()
            {
                diagnostics.unhandled("recovery_chain_protected", result_tokens);
                continue;
            }
            if recent.contains(&exchange.step) && recent_tokens <= soft_budget {
                continue;
            }
            let Some(_) = current_by_id.get(&result.entry.id()) else {
                continue;
            };
            let current_chars = result_costs.get(exchange.call_id).map_or(0, |cost| cost.0);
            let head = if result.is_error { 1536 } else { 512 };
            let tail = if result.is_error { 384 } else { 128 };
            let after = result
                .content
                .preview_text_chars(head, tail)
                .saturating_add(source.archive_ref.chars().count() + 96);
            if after >= current_chars {
                diagnostics.zero_gain_count += 1;
                continue;
            }
            diagnostics.candidate_count += 1;
            diagnostics.preview_count += 1;
            plan.actions.push(ProjectionActionEntry {
                message_id: result.entry.id(),
                target: result.target.clone(),
                action: ProjectionAction::PreviewArchiveRead {
                    archive_ref: source.archive_ref,
                    keep_head: head,
                    keep_tail: tail,
                },
            });
            let saved = ((current_chars - after) as u64) / 4;
            plan.estimated_after_tokens = plan.estimated_after_tokens.saturating_sub(saved);
            if recent.contains(&exchange.step) {
                recent_tokens = recent_tokens.saturating_sub(saved);
            }
            continue;
        }
        if retention == ContextRetention::Report
            && !facts.closed_call_ids.contains(exchange.call_id)
        {
            diagnostics.protect("unsettled_report", result_tokens);
            continue;
        }
        if facts.archive_scope.is_none()
            && transcript
                .projection_ref(result.entry.id())
                .and_then(|directive| directive.archive_catalog.as_ref())
                .is_none()
        {
            diagnostics.unhandled("recovery_unavailable", result_tokens);
            continue;
        }
        if recent.contains(&exchange.step) && recent_tokens <= soft_budget {
            diagnostics.protect("recent_completed_step", result_tokens);
            continue;
        }
        if preview_counts.get(&result.entry.id()).copied().unwrap_or(0) >= MAX_GROUP_EXCHANGES {
            diagnostics.unhandled("inline_preview_action_limit", result_tokens);
            continue;
        }
        diagnostics.candidate_count += 1;
        eligible.insert(ei);
        if !config.has_valid_micro_field_limits() {
            continue;
        }
        let Some(_) = current_by_id.get(&result.entry.id()) else {
            continue;
        };
        let current_chars = result_costs.get(exchange.call_id).map_or(0, |cost| cost.0);
        let canonical_chars = result.content.text_chars();
        // Error previews preserve a larger diagnostic window. Existing shorter previews
        // remain monotone; old truncated markers are never treated as source text.
        let (mut head, mut tail) = if result.is_error {
            (1536, 384)
        } else {
            (
                config.micro_field_keep_head_chars,
                config.micro_field_keep_tail_chars,
            )
        };
        let after_chars = result.content.preview_text_chars(head, tail);
        if canonical_chars <= config.micro_field_threshold_chars || after_chars >= current_chars {
            diagnostics.zero_gain_count += 1;
            continue;
        }
        let Some(reference_chars) = preview_catalogs.reference_chars(transcript, result, facts)?
        else {
            diagnostics.unhandled("recovery_unavailable", result_tokens);
            continue;
        };
        let body_budget = (if result.is_error {
            2048usize
        } else {
            1024usize
        })
        .saturating_sub(reference_chars + 64);
        tail = tail.min(body_budget / 4);
        head = head.min(body_budget.saturating_sub(tail));
        let after_chars = result
            .content
            .preview_text_chars(head, tail)
            .saturating_add(reference_chars);
        if after_chars >= current_chars {
            diagnostics.zero_gain_count += 1;
            continue;
        }
        let savings = ((current_chars - after_chars) as u64) / 4;
        // Catalog discovery is bounded globally by the renderer; charge a conservative
        // one-time root entrance now, then verify actual final request once below.
        if savings == 0 {
            diagnostics.zero_gain_count += 1;
            continue;
        }
        preview_catalogs.mark_used(result.entry.id());
        *preview_counts.entry(result.entry.id()).or_default() += 1;
        plan.actions.push(ProjectionActionEntry {
            message_id: result.entry.id(),
            target: result.target.clone(),
            action: ProjectionAction::CompactToolResult {
                keep_head: head,
                keep_tail: tail,
                preserve_recovery_handle: true,
            },
        });
        diagnostics.preview_count += 1;
        projected_savings.insert(ei, savings);
        plan.estimated_after_tokens = plan.estimated_after_tokens.saturating_sub(savings);
        if recent.contains(&exchange.step) {
            recent_tokens = recent_tokens.saturating_sub(savings);
        }
    }
    diagnostics.preview_catalog_source_scans = preview_catalogs.source_scans;
    let mut catalog_by_anchor = preview_catalogs.into_used();
    let mut archived_actions = Vec::new();
    let mut archived_member_ids = HashSet::new();
    // Only signed reasoning binds the whole batch. Other exchanges can retire
    // independently while their same-message siblings and assistant prose survive.
    for (si, step) in index.steps.iter().enumerate() {
        if plan.estimated_after_tokens <= desired {
            break;
        }
        if step.exchanges.is_empty() || (recent.contains(&si) && recent_tokens <= soft_budget) {
            continue;
        }
        if step.exchanges.len() > MAX_GROUP_EXCHANGES {
            diagnostics.unhandled(
                "archive_group_exchange_limit",
                current_costs.get(&step.entry.id()).copied().unwrap_or(0),
            );
            continue;
        }
        let Some(scope) = &facts.archive_scope else {
            continue;
        };
        if step.unknown
            || (step.protocol_dependency
                && (caps.protocol == ProviderProtocol::Generic
                    || matches!(
                        step.entry.message().message_content(),
                        MessageContent::Raw(_)
                    )))
        {
            diagnostics.unhandled(
                "protocol_dependency",
                current_costs.get(&step.entry.id()).copied().unwrap_or(0),
            );
            continue;
        }
        let mut selected = Vec::new();
        for ei in &step.exchanges {
            if !eligible.contains(ei) {
                continue;
            }
            let exchange = &index.exchanges[*ei];
            let result = exchange.result.as_ref().expect("eligible has a result");
            let unsafe_reason = if !facts.closed_call_ids.contains(exchange.call_id) {
                Some("missing_terminal_evidence")
            } else if !matches!(
                policy(exchange.tool_name, config, facts),
                ContextRetention::Recomputable | ContextRetention::SideEffectReceipt
            ) {
                Some("unknown_side_effect_or_report")
            } else if result.is_error {
                Some("unresolved_error")
            } else if result.content.has_unrecoverable_media() {
                Some("recovery_unavailable_media_or_unknown")
            } else {
                None
            };
            if let Some(reason) = unsafe_reason {
                diagnostics.unhandled(
                    reason,
                    current_costs.get(&result.entry.id()).copied().unwrap_or(0),
                );
            } else {
                selected.push(*ei);
            }
        }
        if selected.is_empty() {
            continue;
        }
        if step.protocol_dependency && selected.len() != step.exchanges.len() {
            diagnostics.unhandled(
                "protocol_dependency",
                current_costs.get(&step.entry.id()).copied().unwrap_or(0),
            );
            continue;
        }
        if !facts.exposed_message_ids.contains(&step.entry.id())
            || facts.protected_message_ids.contains(&step.entry.id())
        {
            continue;
        }
        let mut members = vec![step.entry];
        let mut member_ids = HashSet::from([step.entry.id()]);
        for ei in &selected {
            let result = index.exchanges[*ei]
                .result
                .as_ref()
                .expect("eligible result");
            if member_ids.insert(result.entry.id()) {
                members.push(result.entry);
            }
        }
        if members.len() > peri_acp_types::context_archive::ARCHIVE_MAX_MEMBERS {
            diagnostics.unhandled("archive_group_member_limit", 0);
            continue;
        }
        // Published catalogs are immutable. A later independent subset uses a fresh
        // result anchor instead of mutating an earlier group's source identity.
        let anchor = members
            .iter()
            .skip(1)
            .chain(members.iter().take(1))
            .find(|entry| {
                transcript
                    .projection_ref(entry.id())
                    .and_then(|directive| directive.archive_catalog.as_ref())
                    .is_none()
            })
            .map(|entry| entry.id());
        let Some(anchor) = anchor else {
            diagnostics.unhandled("archive_anchor_already_published", 0);
            continue;
        };
        let group_id = format!("exchange-group:{}", anchor.as_uuid());
        let receipt = receipt_facts(&selected, &index.exchanges, si);
        let payloads = members
            .iter()
            .map(|entry| (*entry).clone().into_payload())
            .collect::<Vec<_>>();
        let catalog = match build_catalog(
            scope.thread_id.clone(),
            group_id.clone(),
            anchor,
            &payloads,
            receipt,
        ) {
            Ok(catalog) => catalog,
            Err(peri_acp_types::context_archive::ArchiveError::Unsupported) => {
                diagnostics.unhandled("recovery_unavailable", 0);
                continue;
            }
            Err(error) => return Err(archive_error(error)),
        };
        let input_tokens: u64 = selected
            .iter()
            .map(|ei| {
                input_costs
                    .get(index.exchanges[*ei].call_id)
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        let result_tokens: u64 = selected
            .iter()
            .map(|ei| {
                result_costs
                    .get(index.exchanges[*ei].call_id)
                    .map_or(0, |cost| cost.1)
                    .saturating_sub(projected_savings.get(ei).copied().unwrap_or(0))
            })
            .sum();
        let remaining = input_tokens.saturating_add(result_tokens);
        let receipt_tokens = (catalog.receipt.chars().count() as u64)
            .div_ceil(4)
            .saturating_add(64);
        if remaining <= receipt_tokens {
            diagnostics.zero_gain_count += 1;
            continue;
        }
        archived_member_ids.extend(member_ids.iter().copied());
        for member_id in &member_ids {
            catalog_by_anchor.remove(member_id);
        }
        for member in members {
            let member_calls = if member.id() == step.entry.id() {
                selected.clone()
            } else {
                selected
                    .iter()
                    .copied()
                    .filter(|ei| {
                        index.exchanges[*ei]
                            .result
                            .as_ref()
                            .is_some_and(|result| result.entry.id() == member.id())
                    })
                    .collect()
            };
            for ei in member_calls {
                archived_actions.push(ProjectionActionEntry {
                    message_id: member.id(),
                    target: ProjectionTarget::ToolCall {
                        tool_call_id: index.exchanges[ei].call_id.to_string(),
                    },
                    action: ProjectionAction::OmitToolExchange {
                        anchor_message_id: anchor,
                        group_id: group_id.clone(),
                        tool_call_ids: vec![index.exchanges[ei].call_id.to_string()],
                    },
                });
            }
        }
        diagnostics.archived_exchange_count += selected.len();
        diagnostics.receipt_count += 1;
        plan.estimated_after_tokens = plan
            .estimated_after_tokens
            .saturating_sub(remaining - receipt_tokens);
        if recent.contains(&si) {
            recent_tokens = recent_tokens.saturating_sub(remaining - receipt_tokens);
        }
        catalog_by_anchor.insert(catalog.anchor_message_id, catalog);
    }
    plan.actions
        .retain(|action| !archived_member_ids.contains(&action.message_id));
    plan.actions.extend(archived_actions);
    let indexed_result_ids = index
        .exchanges
        .iter()
        .filter_map(|exchange| exchange.result.as_ref().map(|result| result.entry.id()))
        .collect::<HashSet<_>>();
    for (position, entry) in transcript.entries().iter().enumerate() {
        if transcript.is_excluded(entry.id()) {
            continue;
        }
        let Some(current_message) = current_by_id.get(&entry.id()) else {
            continue;
        };
        if position < transcript.ancestor_len() {
            diagnostics.protect("ancestor", tokens(current_message));
            continue;
        }
        record_unhandled_content(current_message, &mut diagnostics);
        match entry.as_message() {
            Some(BaseMessage::System { .. }) => {
                diagnostics.protect("system", tokens(current_message))
            }
            Some(BaseMessage::Human { .. }) if !indexed_result_ids.contains(&entry.id()) => {
                diagnostics.protect("user_requirements", tokens(current_message))
            }
            Some(BaseMessage::Ai { .. }) => {
                *diagnostics
                    .remaining_tokens_by_kind
                    .entry("assistant_text".into())
                    .or_default() += (current_message.content().chars().count() as u64).div_ceil(4);
            }
            None => diagnostics.protect(
                "canonical_reminder_without_replacement",
                tokens(current_message),
            ),
            _ => {}
        }
    }
    plan.catalogs = transcript
        .entries()
        .iter()
        .filter_map(|entry| catalog_by_anchor.remove(&entry.id()))
        .collect();
    debug_assert!(
        catalog_by_anchor.is_empty(),
        "catalog anchors come from own history"
    );
    if let (Some(scope), Some(first), Some(last)) = (
        &facts.archive_scope,
        plan.catalogs.first(),
        plan.catalogs.last(),
    ) {
        plan.archive_roots.push(ArchiveRoot {
            source_thread_id: scope.thread_id.clone(),
            first_anchor: first.anchor_message_id,
            last_anchor: last.anchor_message_id,
            committed_tail: scope.committed_tail,
        });
    }
    let mut actions_by_id: HashMap<MessageId, Vec<ProjectionActionEntry>> = HashMap::new();
    for action in std::mem::take(&mut plan.actions) {
        actions_by_id
            .entry(action.message_id)
            .or_default()
            .push(action);
    }
    plan.actions = transcript
        .entries()
        .iter()
        .flat_map(|entry| actions_by_id.remove(&entry.id()).unwrap_or_default())
        .collect();
    debug_assert!(
        actions_by_id.is_empty(),
        "action targets come from own history"
    );
    plan.changed_messages = plan
        .actions
        .iter()
        .map(|action| action.message_id)
        .collect::<HashSet<_>>()
        .len();
    plan.no_op_candidates = diagnostics.zero_gain_count;
    plan.receipt_budget_tokens = receipt_budget;
    if let Err(error) = super::super::micro::validate_plan_metadata(transcript, &plan) {
        if !matches!(
            error,
            peri_acp_types::context_archive::ArchiveError::Unsupported
        ) {
            return Err(archive_error(error));
        }
        tracing::warn!(
            "Micro candidate metadata exceeds bounded recovery capacity; preserving current view"
        );
        diagnostics.unhandled("archive_metadata_budget", before);
        plan.actions.clear();
        plan.catalogs.clear();
        plan.archive_roots.clear();
        plan.changed_messages = 0;
        diagnostics.preview_count = 0;
        diagnostics.receipt_count = 0;
        diagnostics.archived_exchange_count = 0;
        plan.estimated_after_tokens = before;
    }
    if !plan.actions.is_empty() {
        // One final complete rendering validates protocol shape and charges actual
        // receipts/roots. No candidate causes another transcript-wide clone/scan.
        let candidate = render_micro_plan(transcript, &plan, caps)?;
        let after = crate::agent::token::estimate_request_tokens(&candidate, &[])
            .saturating_add(facts.fixed_view_tokens);
        if after >= before {
            diagnostics.zero_gain_count += plan.actions.len();
            plan.actions.clear();
            plan.catalogs.clear();
            plan.archive_roots.clear();
            plan.changed_messages = 0;
            diagnostics.preview_count = 0;
            diagnostics.receipt_count = 0;
            diagnostics.archived_exchange_count = 0;
            plan.estimated_after_tokens = before;
        } else {
            plan.estimated_after_tokens = after;
            remaining_costs(&candidate, transcript, facts, &mut diagnostics);
        }
    }
    if plan.actions.is_empty() {
        remaining_costs(&current, transcript, facts, &mut diagnostics);
    }
    plan.estimated_tokens_saved = before.saturating_sub(plan.estimated_after_tokens);
    diagnostics.target_shortfall_tokens = plan.estimated_after_tokens.saturating_sub(desired);
    plan.diagnostics = diagnostics;
    super::sources::capture(transcript, &mut plan)?;
    Ok(plan)
}

fn remaining_costs(
    messages: &[BaseMessage],
    transcript: &MessageTranscript,
    facts: &MicroPlanningFacts,
    diagnostics: &mut PlanningDiagnostics,
) {
    fn media(block: &crate::messages::ContentBlock) -> u64 {
        use crate::messages::{ContentBlock, DocumentSource, ImageSource};
        fn url_cost(url: &str) -> u64 {
            if url
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
            {
                crate::agent::token::BINARY_MEDIA_ESTIMATED_TOKENS
            } else {
                (url.chars().count() as u64).div_ceil(4)
            }
        }
        match block {
            ContentBlock::Image {
                source: ImageSource::Base64 { .. },
            }
            | ContentBlock::Document {
                source: DocumentSource::Base64 { .. },
                ..
            } => crate::agent::token::BINARY_MEDIA_ESTIMATED_TOKENS,
            ContentBlock::Image {
                source: ImageSource::Url { url },
            }
            | ContentBlock::Document {
                source: DocumentSource::Url { url },
                ..
            } => url_cost(url),
            ContentBlock::Document {
                source: DocumentSource::Text { text },
                ..
            } => (text.chars().count() as u64).div_ceil(4),
            ContentBlock::ToolResult { content, .. } => content.iter().map(media).sum(),
            _ => 0,
        }
    }
    let remaining = &mut diagnostics.remaining_tokens_by_kind;
    remaining.clear();
    remaining.insert("frozen_system_and_tools".into(), facts.fixed_view_tokens);
    for message in messages {
        let total = tokens(message);
        let (kind, input) = match message {
            BaseMessage::Ai { tool_calls, .. } => (
                if transcript.get(message.id()).is_some() {
                    "assistant_text"
                } else {
                    "history_receipts_and_index"
                },
                tool_calls
                    .iter()
                    .map(|call| {
                        ((call.name.chars().count() + call.arguments.to_string().chars().count())
                            as u64)
                            .div_ceil(4)
                    })
                    .sum::<u64>(),
            ),
            BaseMessage::Tool { is_error: true, .. } => ("error", 0),
            BaseMessage::Tool { .. } => ("tool_result", 0),
            BaseMessage::Human { .. } => (
                if transcript
                    .get(message.id())
                    .is_some_and(|entry| entry.as_message().is_none())
                {
                    "reminder"
                } else {
                    "user_content"
                },
                0,
            ),
            BaseMessage::System { .. } => ("system", 0),
        };
        let body = total.saturating_sub(input);
        let media_tokens = match message.message_content() {
            MessageContent::Blocks(blocks) => blocks.iter().map(media).sum::<u64>().min(body),
            _ => 0,
        };
        *remaining.entry("parameters".into()).or_default() += input;
        *remaining.entry("media".into()).or_default() += media_tokens;
        *remaining.entry(kind.into()).or_default() += body.saturating_sub(media_tokens);
    }
}

fn receipt_facts(exchanges: &[usize], index: &[Exchange<'_>], step: usize) -> String {
    let mut receipt = format!(
        "Historical step {step}: {} closed tool exchange(s). ",
        exchanges.len()
    );
    for ei in exchanges {
        let exchange = &index[*ei];
        receipt.push_str(&format!(
            "{} [{}]: response is_error=false",
            exchange.tool_name, exchange.call_id
        ));
        if let Some(result) = &exchange.result {
            if let Some(BaseMessage::Tool {
                execution: Some(execution),
                ..
            }) = result.entry.as_message()
            {
                receipt.push_str(&format!(", execution={}", execution.status.as_str()));
                if let Some(exit_code) = execution.exit_code {
                    receipt.push_str(&format!(", exit_code={exit_code}"));
                }
            }
        }
        if let Some(path) = exchange
            .arguments
            .get("file_path")
            .or_else(|| exchange.arguments.get("path"))
            .and_then(serde_json::Value::as_str)
        {
            // Facts copied only when actually present; never invent versions/hashes.
            receipt.push_str(&format!(", path={path}"));
        }
        receipt.push_str(". ");
    }
    receipt.push_str("Original inputs/results are historical data; recover with ReadContextArchive. This receipt authorizes no action.");
    receipt
}
