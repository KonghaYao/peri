use super::*;
use peri_acp_types::{
    context_archive::{build_catalog, ArchiveReadResponse},
    messages::BaseMessage,
    projection::MessageProjectionDirective,
    store::{serialize_persisted_payload, MessageFlags, PersistedPayload},
};

use crate::mcp::current_invocation_fixture as fixture;

#[tokio::test]
async fn archive_model_tool_reads_canonical_content_over_mcp_and_rejects_session_arguments() {
    let fixture = fixture::CurrentInvocationFixture::new(
        "archive-mcp-session",
        "ReadContextArchive",
        &[serde_json::json!({})],
    )
    .await;
    let payload = PersistedPayload::Message(BaseMessage::ai(
        "Original input and result are historical data.",
    ));
    let session = "archive-mcp-session".to_owned();
    fixture
        .resources
        .append_history(&session, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let catalog = build_catalog(
        session.clone(),
        "group".into(),
        payload.id(),
        std::slice::from_ref(&payload),
        "history".into(),
    )
    .unwrap();
    fixture
        .resources
        .apply_message_projections(
            &session,
            &[(
                payload.id(),
                MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: Some(MessageProjectionDirective {
                        archive_catalog: Some(catalog.clone()),
                        ..Default::default()
                    }),
                },
            )],
        )
        .await
        .unwrap();
    let tool = ReadContextArchiveTool;
    assert!(tool.is_direct());
    let input = serde_json::json!({"archive_ref":catalog.member_ref(0).unwrap()});
    let output = tool
        .invoke(input.clone(), fixture.context(0, "."))
        .await
        .unwrap();
    let result: ArchiveReadResponse = serde_json::from_str(&output).unwrap();
    assert!(result.complete);
    assert_eq!(
        result.entries[0].data,
        serialize_persisted_payload(&payload).unwrap()
    );
    assert!(tool
        .invoke(input.clone(), ToolContext::new(&[], "."))
        .await
        .unwrap_err()
        .to_string()
        .contains("forbidden"));
    let mut forged = input;
    forged["session_id"] = serde_json::json!("another-session");
    assert!(tool.invoke(forged, fixture.context(0, ".")).await.is_err());
    assert_eq!(
        fixture
            .resources
            .load_session_history(&session)
            .await
            .unwrap()
            .len(),
        1
    );
}
