//! 远端有界回退候选读取：与 SQLite adapter 同一套语义，走真实批次与账本。
//!
//! 夹具复用 [`super::session_history_page_tests::remote_with_history`]（同一个
//! `SqliteTransport` 扮演远端服务端）；断言只看调用方可见结果：顺序、上限、归档可见。

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::thread::ThreadId;

use super::session_history_page_tests::{remote_with_history, REWIND_CANDIDATES_SESSION};
use crate::sessions::data::SessionDataPort;

fn texts(candidates: &[peri_acp_types::session_resources::RewindCandidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.text.clone())
        .collect()
}

#[tokio::test]
async fn remote_candidates_are_user_only_newest_first_and_bounded() {
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("第一轮问题")),
        PersistedPayload::Message(BaseMessage::ai("第一轮回答")),
        PersistedPayload::Message(BaseMessage::tool_result(
            "call-1",
            format!("工具结果 {}", "T".repeat(64 * 1024)),
        )),
        PersistedPayload::Message(BaseMessage::human(
            "<system-reminder>后台任务完成通知</system-reminder>",
        )),
        PersistedPayload::Message(BaseMessage::human("第二轮问题")),
        PersistedPayload::Message(BaseMessage::human("第三轮问题")),
    ];
    let (_directory, data) = remote_with_history(&history).await;
    let id: ThreadId = REWIND_CANDIDATES_SESSION.to_owned();

    let candidates = data.load_rewind_candidates(&id, 64).await.unwrap();
    assert_eq!(
        texts(&candidates),
        vec!["第三轮问题", "第二轮问题", "第一轮问题"],
        "只返回 user、最新在前、纯注入不占名额"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate.text.contains("工具结果")),
        "工具正文不得进入候选: {:?}",
        texts(&candidates)
    );
}

#[tokio::test]
async fn remote_candidates_survive_trailing_reminders_across_batches() {
    let mut history: Vec<PersistedPayload> = (0..64)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("valid-{index}"))))
        .collect();
    history.extend((0..130).map(|index| {
        PersistedPayload::Message(BaseMessage::human(format!(
            "<system-reminder>notice-{index}</system-reminder>"
        )))
    }));
    let (_directory, data) = remote_with_history(&history).await;
    let id: ThreadId = REWIND_CANDIDATES_SESSION.to_owned();

    let candidates = data.load_rewind_candidates(&id, 64).await.unwrap();
    assert_eq!(candidates.len(), 64, "尾部注入不挤掉有效候选");
    assert_eq!(candidates[0].text, "valid-63");
    assert_eq!(candidates[63].text, "valid-0");
}

#[tokio::test]
async fn remote_candidates_include_users_archived_by_compaction() {
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("归档问题")),
        PersistedPayload::Message(BaseMessage::ai("回答")),
        PersistedPayload::Message(BaseMessage::human("仍然有效的问题")),
    ];
    let (_directory, data) = remote_with_history(&history).await;
    let id: ThreadId = REWIND_CANDIDATES_SESSION.to_owned();
    data.apply_message_projections(
        &id,
        &[(
            history[0].id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();

    let candidates = data.load_rewind_candidates(&id, 64).await.unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "归档问题"),
        "被归档的 user 消息仍是候选: {:?}",
        texts(&candidates)
    );
    assert_eq!(candidates[0].text, "仍然有效的问题", "最新在前");
}
