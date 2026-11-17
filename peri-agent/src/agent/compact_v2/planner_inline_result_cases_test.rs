// Responsibility-specific cases share the original test namespace and fixtures.

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
    let view = crate::agent::compact_v2::projection::render_micro_plan(
        &transcript,
        &planned,
        &ProviderCapabilities::openai(),
    )
    .expect("the render micro plan fixture result is required for this assertion");
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
    let view = crate::agent::compact_v2::projection::render_micro_plan(
        &transcript,
        &planned,
        &ProviderCapabilities::openai(),
    )
    .expect("the render micro plan fixture result is required for this assertion");
    assert!(view.iter().all(|message| message.tool_calls().is_empty()));
    assert!(view
        .iter()
        .any(|message| message.content().contains("retain this reasoning")));
}

#[test]
fn complete_metadata_budget_preserves_the_old_view_before_publication() {
    use peri_acp_types::context_archive;
    use peri_acp_types::context_archive::ARCHIVE_MAX_METADATA_BYTES;
    let mut transcript = MessageTranscript::new();
    let (_, result_id) = step(&mut transcript, 0, "Read", &"large output".repeat(1000));
    let catalog = context_archive::build_catalog(
        "current-session".into(),
        "existing-preview".into(),
        result_id,
        &[transcript
            .get(result_id)
            .expect("transcript get fixture must contain the requested entry")
            .clone()
            .into_payload()],
        String::new(),
    )
    .expect("the build catalog fixture result is required for this assertion");
    let mut directive = MessageProjectionDirective {
        policy_version: PROJECTION_POLICY_VERSION,
        archive_catalog: Some(catalog),
        ..Default::default()
    };
    let base_size = serde_json::to_vec(&directive)
        .expect("to_vec must accept this test fixture representation")
        .len();
    directive
        .archive_catalog
        .as_mut()
        .expect("the as mut fixture result is required for this assertion")
        .receipt = "x".repeat(ARCHIVE_MAX_METADATA_BYTES - base_size - 96);
    peri_acp_types::context_archive::validate_projection_metadata(&directive)
        .expect("the validate projection metadata fixture result is required for this assertion");
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
