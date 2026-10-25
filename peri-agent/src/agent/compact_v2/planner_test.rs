//! Planner behavior from canonical history through the final model view.
use super::*;
use crate::agent::compact_v2::projection::{
    render_micro_plan, MessageProjectionDirective, MicroCompactPlan, ProjectionAction,
    ProviderCapabilities,
};
use crate::messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest};
use crate::tools::{ToolExecutionEvidence, ToolExecutionStatus};

fn pressure(window: u32) -> ContextPressure {
    ContextPressure {
        estimated_tokens: window as u64 * 3 / 4,
        context_window: window,
        output_reserve: 8000,
        predicted_tool_growth: 0,
        safety_buffer: 0,
        cache_hit_rate: 0.0,
    }
}

fn step(
    transcript: &mut MessageTranscript,
    number: usize,
    tool: &str,
    output: &str,
) -> (MessageId, MessageId) {
    let call = format!("call-{number}");
    let ai = BaseMessage::ai_with_tool_calls(
        "",
        vec![ToolCallRequest::new(
            &call,
            tool,
            serde_json::json!({"path":"historical.rs","body":"source".repeat(2000)}),
        )],
    );
    let ai_id = ai.id();
    transcript.append(ai);
    let result = BaseMessage::tool_result(&call, output);
    let result_id = result.id();
    transcript.append(result);
    (ai_id, result_id)
}

fn facts(transcript: &MessageTranscript, policy: ContextRetention) -> MicroPlanningFacts {
    let mut facts = MicroPlanningFacts {
        archive_scope: transcript
            .entries()
            .last()
            .map(|entry| ArchivePlanningScope {
                thread_id: "current-session".into(),
                committed_tail: entry.id(),
            }),
        ..Default::default()
    };
    for entry in transcript.entries() {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            for call in message.tool_calls() {
                facts.closed_call_ids.insert(call.id.clone());
                facts
                    .tool_retention
                    .insert(call.name.to_lowercase(), policy);
            }
        }
    }
    facts
}

fn plan(transcript: &MessageTranscript, facts: &MicroPlanningFacts) -> MicroCompactPlan {
    plan_micro_with_facts(
        transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &pressure(16_000),
        facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap()
}

#[test]
fn test_pressure_keeps_original_reclaim_and_separates_desired_view() {
    let p = pressure(200_000);
    assert_eq!(p.target_tokens(), 192_000);
    assert_eq!(p.target_reclaim_tokens(), 4_000);
    let transcript = MessageTranscript::new();
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &p,
        &MicroPlanningFacts::default(),
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert_eq!(plan.diagnostics.desired_view_tokens, 90_000);
    assert_eq!(plan.diagnostics.soft_recent_budget_tokens, 8192);
    assert_eq!(plan.diagnostics.receipt_budget_tokens, 2048);
}

#[test]
fn test_pressure_reserves_saturate_without_changing_full_floor() {
    let p = ContextPressure {
        output_reserve: u32::MAX,
        predicted_tool_growth: 1,
        ..pressure(16_000)
    };
    assert_eq!(p.target_tokens(), 0);
    assert_eq!(p.target_reclaim_tokens(), 12_000);
}

#[test]
fn test_one_user_hundred_steps_archive_old_history_with_default_age() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("Complete this one task."));
    for i in 0..100 {
        step(&mut transcript, i, "Read", &"old result".repeat(3000));
    }
    let facts = facts(&transcript, ContextRetention::Recomputable);
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(16_000),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    println!(
        "micro_100_step_fixture candidates={} archived={} before={} after={} delta={}",
        plan.diagnostics.candidate_count,
        plan.diagnostics.archived_exchange_count,
        plan.estimated_before_tokens,
        plan.estimated_after_tokens,
        plan.estimated_tokens_saved
    );
    assert!(
        plan.diagnostics.candidate_count >= 98,
        "年龄按模型步骤而非用户发言计数"
    );
    assert!(plan.diagnostics.archived_exchange_count >= 98);
    assert!(
        plan.estimated_tokens_saved * 100 / plan.estimated_before_tokens >= 80,
        "固定样本可回收部分至少下降80%"
    );
    assert!(view
        .iter()
        .any(|message| message.content() == "Complete this one task."));
    assert_eq!(transcript.entries().len(), 201, "canonical 原文不可更改");
}

#[test]
fn test_cold_history_has_no_inferred_exposure() {
    let mut transcript = MessageTranscript::new();
    for i in 0..5 {
        step(&mut transcript, i, "Read", &"result".repeat(2000));
    }
    let plan = plan(&transcript, &MicroPlanningFacts::default());
    assert!(plan.actions.is_empty());
    assert!(plan.diagnostics.protected_tokens_by_reason["not_exposed"] > 0);
}

#[test]
fn test_cross_human_interjection_pairs_by_identity() {
    let mut transcript = MessageTranscript::new();
    let ai = BaseMessage::ai_with_tool_calls(
        "",
        vec![ToolCallRequest::new(
            "a",
            "Read",
            serde_json::json!({"path":"a"}),
        )],
    );
    let ai_id = ai.id();
    transcript.append(ai);
    let user = BaseMessage::human("Do not change the public API.");
    let user_id = user.id();
    transcript.append(user);
    transcript.append(BaseMessage::tool_result("a", "source".repeat(3000)));
    let mut facts = facts(&transcript, ContextRetention::Recomputable);
    // The complete request remains over the desired budget even after result preview.
    facts.fixed_view_tokens = 8_000;
    let plan = plan(&transcript, &facts);
    assert!(plan.estimated_before_tokens > plan.diagnostics.desired_view_tokens);
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert!(plan.actions.iter().any(|action| action.message_id == ai_id));
    assert!(view
        .iter()
        .any(|message| message.id() == user_id && message.content().contains("public API")));
}

#[test]
fn test_duplicate_call_identity_is_explicit_error() {
    let mut transcript = MessageTranscript::new();
    for _ in 0..2 {
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new("same", "Read", serde_json::json!({}))],
        ));
    }
    let error = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(16_000),
        &MicroPlanningFacts::default(),
        &ProviderCapabilities::openai(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("duplicate call ID"));
}

#[test]
fn test_conflicting_cached_and_raw_call_is_rejected() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::Ai { id: MessageId::new(), content: MessageContent::Raw(vec![serde_json::json!({"type":"tool_use","id":"a","name":"Read","input":{"path":"different"}})]), tool_calls: vec![ToolCallRequest::new("a", "Read", serde_json::json!({"path":"original"}))] });
    let error = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(16_000),
        &MicroPlanningFacts::default(),
        &ProviderCapabilities::openai(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("conflicting representations"));
}

#[test]
fn test_unknown_tool_preview_cannot_erase_side_effect_input() {
    let mut transcript = MessageTranscript::new();
    let (ai_id, result_id) = step(&mut transcript, 0, "UnknownTool", &"result".repeat(3000));
    let facts = facts(&transcript, ContextRetention::PreviewOnly);
    let plan = plan(&transcript, &facts);
    assert!(plan
        .actions
        .iter()
        .any(|entry| entry.message_id == result_id
            && matches!(entry.action, ProjectionAction::CompactToolResult { .. })));
    assert!(!plan.actions.iter().any(|entry| entry.message_id == ai_id));
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
}

#[test]
fn test_missing_recovery_blocks_preview_and_group_archive() {
    let mut transcript = MessageTranscript::new();
    step(&mut transcript, 0, "Read", &"result".repeat(3000));
    let mut facts = facts(&transcript, ContextRetention::Recomputable);
    facts.archive_scope = None;
    let plan = plan(&transcript, &facts);
    assert!(plan.actions.is_empty());
    assert!(plan.diagnostics.unhandled_by_reason["recovery_unavailable"] > 0);
}

#[test]
fn test_running_receipt_and_unconsumed_terminal_are_hard_protected() {
    let mut transcript = MessageTranscript::new();
    let (_, first) = step(&mut transcript, 0, "Bash", &"running".repeat(3000));
    let mut message = transcript.get(first).unwrap().message().clone();
    if let BaseMessage::Tool { execution, .. } = &mut message {
        *execution = Some(ToolExecutionEvidence {
            status: ToolExecutionStatus::Running,
            exit_code: None,
            output_ref: None,
            output_truncated: false,
            task_id: Some("task".into()),
        });
    }
    transcript.replace_by_id(message);
    let (_, second) = step(&mut transcript, 1, "Bash", &"terminal".repeat(3000));
    let mut facts = facts(&transcript, ContextRetention::SideEffectReceipt);
    facts.protected_message_ids.insert(second);
    let plan = plan(&transcript, &facts);
    assert!(plan.actions.is_empty());
    assert!(plan.diagnostics.protected_tokens_by_reason["unsettled_execution"] > 0);
    assert!(plan.diagnostics.protected_tokens_by_reason["protected_result"] > 0);
}

#[test]
fn test_similar_error_prefixes_remain_distinct_diagnostics() {
    let mut transcript = MessageTranscript::new();
    for (i, diagnosis) in ["E0308", "E0599"].iter().enumerate() {
        let (_, id) = step(
            &mut transcript,
            i,
            "Bash",
            &format!("{}\n{diagnosis}", "same prefix".repeat(3000)),
        );
        let mut message = transcript.get(id).unwrap().message().clone();
        if let BaseMessage::Tool { is_error, .. } = &mut message {
            *is_error = true;
        }
        transcript.replace_by_id(message);
    }
    let facts = facts(&transcript, ContextRetention::SideEffectReceipt);
    let plan = plan(&transcript, &facts);
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
    assert!(view
        .iter()
        .any(|message| message.content().contains("E0308")));
    assert!(view
        .iter()
        .any(|message| message.content().contains("E0599")));
    assert_eq!(plan.actions.len(), 2);
}

#[test]
fn test_mixed_open_batch_retires_only_closed_independent_exchange() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "The decision is still valid.",
        vec![
            ToolCallRequest::new("closed", "Read", serde_json::json!({})),
            ToolCallRequest::new("open", "Read", serde_json::json!({})),
        ],
    ));
    transcript.append(BaseMessage::tool_result(
        "closed",
        "old result".repeat(3000),
    ));
    let mut facts = facts(&transcript, ContextRetention::Recomputable);
    facts.closed_call_ids.remove("open");
    facts.fixed_view_tokens = 8_000;
    let plan = plan(&transcript, &facts);
    assert!(plan.estimated_before_tokens > plan.diagnostics.desired_view_tokens);
    assert_eq!(plan.diagnostics.archived_exchange_count, 1);
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert!(view
        .iter()
        .flat_map(|message| message.tool_calls())
        .any(|call| call.id == "open"));
    assert!(!view
        .iter()
        .flat_map(|message| message.tool_calls())
        .any(|call| call.id == "closed"));
    assert!(view
        .iter()
        .any(|message| message.content().contains("decision is still valid")));
}

#[test]
fn test_pure_persisted_media_is_candidate_without_text_length() {
    let mut transcript = MessageTranscript::new();
    let (_, result_id) = step(&mut transcript, 0, "Read", "");
    let mut message = transcript.get(result_id).unwrap().message().clone();
    if let BaseMessage::Tool { content, .. } = &mut message {
        *content = MessageContent::blocks(vec![
            ContentBlock::image_base64("image/png", "AQID"),
            ContentBlock::Document {
                source: crate::messages::DocumentSource::Text {
                    text: "document".repeat(5000),
                },
                title: Some("original".into()),
            },
        ]);
    }
    transcript.replace_by_id(message);
    let plan = plan(
        &transcript,
        &facts(&transcript, ContextRetention::Recomputable),
    );
    assert!(plan.diagnostics.candidate_count > 0);
    assert_eq!(plan.diagnostics.archived_exchange_count, 1);
    assert!(plan.catalogs.iter().any(|catalog| catalog
        .members
        .iter()
        .any(|member| member.message_id == result_id)));
}

#[test]
fn test_unknown_block_preserved_with_explicit_coverage_gap() {
    let mut transcript = MessageTranscript::new();
    let (_, result_id) = step(&mut transcript, 0, "Read", "");
    let mut message = transcript.get(result_id).unwrap().message().clone();
    if let BaseMessage::Tool { content, .. } = &mut message {
        *content = MessageContent::blocks(vec![
            ContentBlock::text("text".repeat(5000)),
            ContentBlock::Unknown(serde_json::json!({"type":"future","secret":"opaque"})),
        ]);
    }
    transcript.replace_by_id(message);
    let plan = plan(
        &transcript,
        &facts(&transcript, ContextRetention::Recomputable),
    );
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert!(plan.diagnostics.unhandled_by_reason["unknown_block"] > 0);
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
    assert!(view.iter().any(|message| matches!(message.message_content(), MessageContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Unknown(_))))));
}

#[test]
fn provider_replay_preserves_whole_call_result_step_under_micro_pressure() {
    let mut transcript = MessageTranscript::new();
    let (assistant_id, _) = step(&mut transcript, 0, "Read", &"large result".repeat(5000));
    let mut assistant = transcript.get(assistant_id).unwrap().message().clone();
    if let BaseMessage::Ai { content, .. } = &mut assistant {
        *content = MessageContent::blocks(vec![ContentBlock::ProviderState {
            provider: "openai_responses".into(),
            data: serde_json::json!({"version":1,"items":[{"type":"reasoning","encrypted_content":"opaque"}]}),
        }]);
    }
    transcript.replace_by_id(assistant);
    let expected = serde_json::to_value(transcript.visible_messages()).unwrap();
    let plan = plan(
        &transcript,
        &facts(&transcript, ContextRetention::Recomputable),
    );
    assert!(plan.actions.is_empty());
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert_eq!(serde_json::to_value(&view).unwrap(), expected);
    assert!(view[0].message_content().has_provider_state());
}

#[test]
fn test_small_window_recent_budget_scales_and_does_not_preserve_entire_task() {
    let mut transcript = MessageTranscript::new();
    step(&mut transcript, 0, "Read", &"large recent".repeat(5000));
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(4000),
        &facts(&transcript, ContextRetention::Recomputable),
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert_eq!(plan.diagnostics.soft_recent_budget_tokens, 400);
    assert_eq!(plan.diagnostics.receipt_budget_tokens, 80);
    assert!(!plan.actions.is_empty());
}

#[test]
fn test_short_history_never_inflates_to_receipts() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "",
        vec![ToolCallRequest::new("a", "Read", serde_json::json!({}))],
    ));
    transcript.append(BaseMessage::tool_result("a", "ok"));
    let plan = plan(
        &transcript,
        &facts(&transcript, ContextRetention::Recomputable),
    );
    assert!(plan.actions.is_empty());
    assert_eq!(plan.estimated_tokens_saved, 0);
}

#[test]
fn test_already_previewed_history_can_advance_without_recounting_original() {
    let mut transcript = MessageTranscript::new();
    let (_, result_id) = step(&mut transcript, 0, "Read", &"result".repeat(5000));
    let facts = facts(&transcript, ContextRetention::Recomputable);
    let action = crate::agent::compact_v2::ProjectionActionEntry {
        message_id: result_id,
        target: crate::agent::compact_v2::ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 1000,
            keep_tail: 300,
            preserve_recovery_handle: true,
        },
    };
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![action],
            ..Default::default()
        },
    );
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &pressure(10_000),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert!(plan.estimated_before_tokens > plan.diagnostics.desired_view_tokens);
    assert_eq!(plan.diagnostics.archived_exchange_count, 1);
    assert!(
        plan.estimated_before_tokens < 6000,
        "收益必须基于现有预览，不重复计算canonical正文"
    );
    assert!(plan.estimated_tokens_saved <= plan.estimated_before_tokens);
}

#[test]
fn test_user_and_fixed_budget_shortfall_are_reported_without_erasing_them() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("constraint".repeat(1000)));
    let facts = MicroPlanningFacts {
        fixed_view_tokens: 50_000,
        ..Default::default()
    };
    let plan = plan(&transcript, &facts);
    assert!(plan.actions.is_empty());
    assert_eq!(
        plan.diagnostics.protected_tokens_by_reason["frozen_system_and_tools"],
        50_000
    );
    assert!(plan.diagnostics.protected_tokens_by_reason["user_requirements"] > 0);
}

#[test]
fn test_archive_read_preview_reuses_source_reference_without_catalog_chain() {
    use peri_acp_types::context_archive::{ArchiveReference, ArchiveRoot};
    let mut transcript = MessageTranscript::new();
    let source = MessageId::new();
    let archive_ref = ArchiveReference::Range {
        root: ArchiveRoot {
            source_thread_id: "current-session".into(),
            first_anchor: source,
            last_anchor: source,
            committed_tail: source,
        },
    }
    .encode()
    .unwrap();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "",
        vec![ToolCallRequest::new(
            "retrieve",
            "ReadContextArchive",
            serde_json::json!({"archive_ref":archive_ref}),
        )],
    ));
    let result = BaseMessage::tool_result("retrieve", "recovered historical fragment".repeat(3000));
    let result_id = result.id();
    transcript.append(result);
    let mut facts = facts(&transcript, ContextRetention::ArchiveRead);
    facts.archive_scope = None;
    let plan = plan(&transcript, &facts);
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
    assert!(plan.catalogs.is_empty());
    assert!(plan.archive_roots.is_empty());
    assert!(
        matches!(&plan.actions[0].action, ProjectionAction::PreviewArchiveRead { archive_ref: actual, .. } if actual == &archive_ref)
    );
    assert!(view
        .iter()
        .any(|message| message.id() == result_id && message.content().contains(&archive_ref)));
}

#[test]
fn test_signed_batch_keeps_reasoning_when_one_result_is_not_exposed() {
    let mut transcript = MessageTranscript::new();
    let a = ToolCallRequest::new("a", "Read", serde_json::json!({}));
    let b = ToolCallRequest::new("b", "Read", serde_json::json!({}));
    transcript.append(BaseMessage::ai_with_tool_calls(
        MessageContent::blocks(vec![
            ContentBlock::reasoning_with_signature("private thought", "original-signature"),
            ContentBlock::tool_use("a", "Read", serde_json::json!({})),
            ContentBlock::tool_use("b", "Read", serde_json::json!({})),
        ]),
        vec![a, b],
    ));
    transcript.append(BaseMessage::tool_result(
        "a",
        "completed output".repeat(3000),
    ));
    let fresh = BaseMessage::tool_result("b", "new output".repeat(3000));
    let fresh_id = fresh.id();
    transcript.append(fresh);
    let mut facts = facts(&transcript, ContextRetention::Recomputable);
    facts.exposed_message_ids.remove(&fresh_id);
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &pressure(16_000),
        &facts,
        &ProviderCapabilities::anthropic(),
    )
    .unwrap();
    let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::anthropic()).unwrap();
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
    assert!(view.iter().any(|message| message.content_blocks().iter().any(|block| matches!(block, ContentBlock::Reasoning { signature: Some(signature), .. } if signature == "original-signature"))));
    assert!(view
        .iter()
        .any(|message| message.id() == fresh_id && message.content() == "new output".repeat(3000)));
}

#[test]
fn test_index_understands_raw_calls_and_inline_results_across_user_content() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::Ai { id: MessageId::new(), content: MessageContent::raw(vec![serde_json::json!({"type":"tool_use","id":"raw","name":"Read","input":{"path":"past.rs"}})]), tool_calls: Vec::new() });
    transcript.append(BaseMessage::human("additional constraint"));
    transcript.append(BaseMessage::human(MessageContent::raw(vec![serde_json::json!({"type":"tool_result","tool_use_id":"raw","content":[{"type":"text","text":"actual historical result"}]}), serde_json::json!({"type":"text","text":"retain user text"})])));
    let index = index::HistoryIndex::collect(&transcript).unwrap();
    assert_eq!(index.exchanges.len(), 1);
    let result = index.exchanges[0].result.as_ref().unwrap();
    assert_eq!(index.exchanges[0].call_id, "raw");
    assert_eq!(
        result.content.text_chars(),
        "actual historical result".len()
    );
}

#[test]
fn test_ten_thousand_exchanges_have_bounded_index_and_net_projection() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("One long task."));
    let output = "output".repeat(400);
    for i in 0..10_000 {
        let id = format!("call-{i}");
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                &id,
                "Read",
                serde_json::json!({"path":"fixed.rs"}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(&id, output.clone()));
    }
    let facts = facts(&transcript, ContextRetention::Recomputable);
    let source_bytes = transcript
        .entries()
        .iter()
        .filter_map(|entry| entry.as_message())
        .map(|message| (message.id(), serde_json::to_vec(message).unwrap().len()))
        .collect::<std::collections::HashMap<_, _>>();
    let canonical_serialized_bytes: usize = source_bytes.values().sum();
    let start = std::time::Instant::now();
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(16_000),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    let elapsed = start.elapsed();
    let source_fingerprint_bytes: usize = plan
        .source_content
        .iter()
        .map(|(id, _)| source_bytes[id])
        .sum();
    assert_eq!(
        plan.source_content
            .iter()
            .map(|(id, _)| id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        plan.source_content.len(),
        "each source is fingerprinted once"
    );
    println!("micro_planner_fixture candidates={} archived={} scanned_messages={} canonical_body_bytes={} canonical_serialized_bytes={} source_fingerprint_bytes={} source_hash_count={} before={} after={} delta={} plan_elapsed_ms={}",
        plan.diagnostics.candidate_count, plan.diagnostics.archived_exchange_count,
        plan.diagnostics.scanned_messages, 24_000_000, canonical_serialized_bytes,
        source_fingerprint_bytes, plan.source_content.len(), plan.estimated_before_tokens,
        plan.estimated_after_tokens, plan.estimated_tokens_saved, elapsed.as_millis());
    assert_eq!(plan.diagnostics.scanned_messages, 20_001);
    assert!(plan.diagnostics.archived_exchange_count >= 9998);
    assert!(plan.archive_roots.len() <= 1);
    assert!(
        plan.estimated_after_tokens < 10_000,
        "历史目录入口不随收据数量线性增长"
    );
}

#[test]
fn test_strong_levels_stop_after_desired_view_is_reached() {
    let mut transcript = MessageTranscript::new();
    for i in 0..8 {
        step(&mut transcript, i, "Read", &"history".repeat(12000));
    }
    let facts = facts(&transcript, ContextRetention::Recomputable);
    let roomy = plan_micro_with_facts(
        &transcript,
        &CompactConfig::default(),
        &pressure(200_000),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert!(roomy.diagnostics.preview_count > 0);
    assert_eq!(
        roomy.diagnostics.archived_exchange_count, 0,
        "previews already satisfy the desired view budget"
    );
    let tight = plan(&transcript, &facts);
    assert!(
        tight.diagnostics.archived_exchange_count > 0,
        "smaller desired budget escalates closed exchanges"
    );
    assert!(tight.estimated_after_tokens < roomy.estimated_after_tokens);
}

#[test]
fn test_report_requires_closed_consumed_evidence_and_never_becomes_fact_summary() {
    let mut transcript = MessageTranscript::new();
    let (call_id, result_id) = step(
        &mut transcript,
        0,
        "Agent",
        &format!(
            "{}\nReport conclusion remains distinct",
            "research note ".repeat(3000)
        ),
    );
    let mut evidence = facts(&transcript, ContextRetention::Report);
    evidence.closed_call_ids.clear();
    let open = plan(&transcript, &evidence);
    assert!(open.actions.is_empty());
    assert!(open.diagnostics.protected_tokens_by_reason["unsettled_report"] > 0);
    evidence.closed_call_ids.insert("call-0".into());
    evidence.exposed_message_ids.remove(&result_id);
    assert!(
        plan(&transcript, &evidence).actions.is_empty(),
        "unconsumed report stays whole"
    );
    evidence.exposed_message_ids.insert(result_id);
    let closed = plan(&transcript, &evidence);
    assert!(closed
        .actions
        .iter()
        .any(|action| action.message_id == result_id
            && matches!(action.action, ProjectionAction::CompactToolResult { .. })));
    assert!(!closed
        .actions
        .iter()
        .any(|action| action.message_id == call_id));
    assert_eq!(closed.diagnostics.archived_exchange_count, 0);
    let view = render_micro_plan(&transcript, &closed, &ProviderCapabilities::openai()).unwrap();
    assert!(view.iter().any(|message| message
        .content()
        .contains("Report conclusion remains distinct")));
    let mut unknown = transcript.get(result_id).unwrap().message().clone();
    if let BaseMessage::Tool { execution, .. } = &mut unknown {
        *execution = Some(ToolExecutionEvidence {
            status: ToolExecutionStatus::Unknown,
            task_id: None,
            exit_code: None,
            output_ref: None,
            output_truncated: false,
        });
    }
    transcript.replace_by_id(unknown);
    assert!(
        plan(&transcript, &evidence).actions.is_empty(),
        "unknown execution stays protected even with stale closed facts"
    );
}

#[test]
fn test_preview_below_desired_does_not_claim_required_reclaim_was_met() {
    let mut transcript = MessageTranscript::new();
    step(&mut transcript, 0, "Read", &"result".repeat(1000));
    let facts = facts(&transcript, ContextRetention::Recomputable);
    let pressure = ContextPressure {
        estimated_tokens: 196_000,
        ..pressure(200_000)
    };
    let plan = plan_micro_with_facts(
        &transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &pressure,
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert!(plan.estimated_before_tokens < plan.diagnostics.desired_view_tokens);
    assert!(plan.diagnostics.preview_count > 0);
    assert_eq!(plan.diagnostics.archived_exchange_count, 0);
    assert_eq!(plan.target_reclaim_tokens, pressure.target_reclaim_tokens());
    assert!(plan.estimated_tokens_saved > 0);
    assert!(!plan.meets_target(), "required reclaim remains independent of the desired view; the 95% Full gate retains its decision");
}

fn inline_batch(count: usize) -> MessageTranscript {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "retain this reasoning",
        (0..count)
            .map(|i| {
                ToolCallRequest::new(
                    format!("inline-{i:04}"),
                    "Read",
                    serde_json::json!({"path":format!("source-{i}.rs"),"options":[true,7,null]}),
                )
            })
            .collect(),
    ));
    transcript.append(BaseMessage::human(MessageContent::blocks(
        (0..count)
            .map(|i| ContentBlock::ToolResult {
                id: Some(format!("native-{i}")),
                tool_use_id: format!("inline-{i:04}"),
                content: vec![ContentBlock::text(format!(
                    "result {i}: {}",
                    "body".repeat(2000)
                ))],
                is_error: false,
            })
            .collect(),
    )));
    transcript
}

#[test]
fn shared_inline_results_build_one_catalog_and_bound_batch_actions() {
    let transcript = inline_batch(128);
    let mut evidence = facts(&transcript, ContextRetention::Recomputable);
    evidence.fixed_view_tokens = 8_000;
    let planned = plan(&transcript, &evidence);
    assert_eq!(planned.diagnostics.preview_catalog_source_scans, 1);
    assert_eq!(planned.catalogs.len(), 1);
    assert_eq!(planned.actions.len(), 32);
    assert_eq!(planned.diagnostics.archived_exchange_count, 0);
    assert!(planned.diagnostics.unhandled_by_reason["inline_preview_action_limit"] > 0);
    assert!(planned.diagnostics.unhandled_by_reason["archive_group_exchange_limit"] > 0);
    assert_eq!(
        planned.source_content.len(),
        1,
        "one canonical result source is fingerprinted"
    );
    let view = render_micro_plan(&transcript, &planned, &ProviderCapabilities::openai()).unwrap();
    assert_eq!(
        view[0].tool_calls(),
        transcript.entries()[0].message().tool_calls()
    );
    let original = transcript.entries()[1].message().content_blocks();
    let retained = view[1].content_blocks();
    assert_eq!(
        &retained[32..],
        &original[32..],
        "unadmitted result IDs and content stay intact"
    );
    assert!(planned.estimated_after_tokens < planned.estimated_before_tokens);
}

#[test]
fn closed_inline_batch_uses_linear_call_identity_payloads() {
    let transcript = inline_batch(16);
    let mut evidence = facts(&transcript, ContextRetention::Recomputable);
    evidence.fixed_view_tokens = 8_000;
    let planned = plan(&transcript, &evidence);
    assert_eq!(planned.diagnostics.archived_exchange_count, 16);
    assert_eq!(planned.actions.len(), 32);
    for action in &planned.actions {
        let ProjectionAction::OmitToolExchange { tool_call_ids, .. } = &action.action else {
            panic!("closed batch must retire as a protocol group");
        };
        let crate::agent::compact_v2::projection::ProjectionTarget::ToolCall { tool_call_id } =
            &action.target
        else {
            panic!("each member action is tied to one call identity");
        };
        assert_eq!(tool_call_ids.as_slice(), std::slice::from_ref(tool_call_id));
    }
    let view = render_micro_plan(&transcript, &planned, &ProviderCapabilities::openai()).unwrap();
    assert!(view.iter().all(|message| message.tool_calls().is_empty()));
    assert!(view
        .iter()
        .any(|message| message.content().contains("retain this reasoning")));
}

#[test]
fn complete_metadata_budget_preserves_the_old_view_before_publication() {
    use peri_acp_types::context_archive::{build_catalog, ARCHIVE_MAX_METADATA_BYTES};
    let mut transcript = MessageTranscript::new();
    let (_, result_id) = step(&mut transcript, 0, "Read", &"large output".repeat(1000));
    let catalog = build_catalog(
        "current-session".into(),
        "existing-preview".into(),
        result_id,
        &[transcript.get(result_id).unwrap().clone().into_payload()],
        String::new(),
    )
    .unwrap();
    let mut directive = MessageProjectionDirective {
        policy_version: PROJECTION_POLICY_VERSION,
        archive_catalog: Some(catalog),
        ..Default::default()
    };
    let base_size = serde_json::to_vec(&directive).unwrap().len();
    directive.archive_catalog.as_mut().unwrap().receipt =
        "x".repeat(ARCHIVE_MAX_METADATA_BYTES - base_size - 96);
    peri_acp_types::context_archive::validate_projection_metadata(&directive).unwrap();
    transcript.set_flags_projection(result_id, directive.clone());
    let evidence = facts(&transcript, ContextRetention::PreviewOnly);
    let planned = plan(&transcript, &evidence);
    assert!(planned.actions.is_empty() && planned.catalogs.is_empty());
    assert_eq!(
        planned.estimated_before_tokens,
        planned.estimated_after_tokens
    );
    assert!(planned.diagnostics.unhandled_by_reason["archive_metadata_budget"] > 0);
    assert_eq!(transcript.projection_ref(result_id), Some(&directive));
}

/// [回归测试] typed 失败只证明调用已结算，缺少 is_error 也不能整组省略失败。
#[test]
fn test_typed_unsuccessful_terminal_without_error_flag_is_not_wholly_archived() {
    for status in [
        ToolExecutionStatus::Failed,
        ToolExecutionStatus::Cancelled,
        ToolExecutionStatus::TimedOut,
    ] {
        let mut transcript = MessageTranscript::new();
        let (ai, result) = step(&mut transcript, 0, "Write", &"failed detail ".repeat(4_000));
        let mut message = transcript.get(result).unwrap().message().clone();
        if let BaseMessage::Tool {
            execution,
            is_error,
            ..
        } = &mut message
        {
            assert!(!*is_error, "夹具保留不一致的历史 outcome 标志");
            *execution = Some(ToolExecutionEvidence {
                status,
                task_id: None,
                exit_code: None,
                output_ref: None,
                output_truncated: false,
            });
        }
        transcript.replace_by_id(message);
        transcript.append(BaseMessage::human("retained requirement ".repeat(5_000)));
        let facts = facts(&transcript, ContextRetention::SideEffectReceipt);
        let plan = plan(&transcript, &facts);
        assert_eq!(plan.diagnostics.archived_exchange_count, 0, "{status:?}");
        assert!(plan.diagnostics.unhandled_by_reason["unresolved_error"] > 0);
        let view = render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai()).unwrap();
        assert!(view.iter().any(|message| message.id() == ai));
        assert!(view.iter().any(|message| message.id() == result));
    }
}
