use super::*;
use crate::context_archive::ContextArchiveReader;
use peri_acp_types::context_archive::{
    content_digest, ArchiveReadRequest, ContextArchiveReadPort, ARCHIVE_MAX_TOKENS,
};
use peri_acp_types::messages::{ContentBlock, MessageContent};
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};

/// [回归测试] 大量同类型 block 曾使归档响应仅元数据就超过最大预算。
#[tokio::test]
async fn test_micro_many_blocks_commit_and_recover_every_canonical_byte() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    transcript.append(BaseMessage::human("保留所有原文"));
    let mut original = None;
    for i in 0..6 {
        let call = format!("blocks-{i}");
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                &call,
                "Read",
                serde_json::json!({"path":"old.rs"}),
            )],
        ));
        let result = if i == 0 {
            BaseMessage::tool_result(
                &call,
                MessageContent::Blocks(
                    (0..2500)
                        .map(|_| ContentBlock::text("a".repeat(600)))
                        .collect(),
                ),
            )
        } else {
            BaseMessage::tool_result(&call, "small result")
        };
        if i == 0 {
            original = Some(PersistedPayload::Message(result.clone()));
        }
        transcript.append(result);
    }
    let original = original.unwrap();
    let plan = exposed_plan(&transcript, bound.thread_id());
    let catalog = plan
        .catalogs
        .iter()
        .find(|catalog| {
            catalog
                .members
                .iter()
                .any(|m| m.message_id == original.id())
        })
        .expect("大结果必须实际归档");
    let index = catalog
        .members
        .iter()
        .position(|m| m.message_id == original.id())
        .unwrap();
    assert_eq!(catalog.members[index].block_types, vec!["text"]);
    let reference = catalog.member_ref(index).unwrap();
    let expected_digest = catalog.members[index].content_digest.clone();
    assert!(apply_micro_plan(&mut transcript, &plan).await.unwrap() > 0);
    let reader = ContextArchiveReader::new(bound.read_only_resources().await, bound.thread_id());
    let mut recovered = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = reader
            .read(ArchiveReadRequest {
                archive_ref: reference.clone(),
                cursor,
                budget_tokens: Some(ARCHIVE_MAX_TOKENS),
            })
            .await
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= ARCHIVE_MAX_TOKENS as usize * 4);
        for entry in page.entries {
            assert_eq!(entry.offset, recovered.len() as u64);
            assert_eq!(entry.block_types, vec!["text"]);
            assert_eq!(entry.encoding, "utf8");
            recovered.extend_from_slice(entry.data.as_bytes());
        }
        pages += 1;
        if page.complete {
            break;
        }
        cursor = page.next_cursor;
        assert!(pages < 1000, "分页必须推进");
    }
    assert!(pages > 1);
    assert_eq!(
        recovered,
        serialize_persisted_payload(&original).unwrap().as_bytes()
    );
    assert_eq!(content_digest(&recovered), expected_digest);
}
