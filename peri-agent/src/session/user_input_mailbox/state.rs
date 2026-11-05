//! 运行期记录、命令回执与派生辅助。
//!
//! 记录不复制 MQ 的权威状态，只为纯内存投影缓存正文与原稿；显式
//! `refresh_from_store` 用查询结果补齐或清理这些记录。

use std::hash::{Hash, Hasher};

use peri_acp_types::{
    messages::{BaseMessage, MessageContent, MessageId},
    session::{MqEntry, UserInput, UserInputItemResult, UserInputState},
    store::PersistedPayload,
};

use super::{CommandReceipt, InputRecord, MailboxState, UserInputQueueError, UserInputRunTicket};

impl MailboxState {
    pub(super) fn next_record_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }
}

impl InputRecord {
    pub(super) fn new(
        input_id: String,
        fingerprint: u64,
        seq: u64,
        content: MessageContent,
        original_draft: String,
    ) -> Self {
        Self {
            input_id,
            fingerprint,
            seq,
            content,
            original_draft,
            state: UserInputState::Queued,
            handed_off: false,
            delivery_reserved: true,
        }
    }
}

pub(super) fn new_ticket() -> UserInputRunTicket {
    UserInputRunTicket {
        id: uuid::Uuid::now_v7().to_string(),
    }
}

pub(super) fn validate_input_id(id: &str) -> Result<(), UserInputQueueError> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| UserInputQueueError::InvalidIdentity)?;
    if parsed.to_string() != id {
        return Err(UserInputQueueError::InvalidIdentity);
    }
    Ok(())
}

pub(super) fn remember_command(
    state: &mut MailboxState,
    command: &str,
    fingerprint: u64,
    results: Vec<UserInputItemResult>,
    taken_back: Option<UserInput>,
) {
    state.commands.insert(
        command.to_owned(),
        CommandReceipt {
            fingerprint,
            results,
            taken_back,
        },
    );
}

pub(super) fn results_for(state: &MailboxState, ids: &[String]) -> Vec<UserInputItemResult> {
    ids.iter()
        .map(|id| UserInputItemResult {
            input_id: id.clone(),
            state: state
                .records
                .iter()
                .find(|record| record.input_id == *id)
                .map(|record| record.state)
                .or_else(|| {
                    uuid::Uuid::parse_str(id)
                        .ok()
                        .and_then(|input_uuid| state.terminal_identities.get(&input_uuid))
                        .map(|identity| identity.state)
                })
                .unwrap_or(UserInputState::Unknown),
        })
        .collect()
}

pub(super) fn is_pending(state: UserInputState) -> bool {
    matches!(
        state,
        UserInputState::Queued | UserInputState::Dispatching | UserInputState::Claimed
    )
}

pub(super) fn matches_id(record: &InputRecord, ids: &[MessageId]) -> bool {
    uuid::Uuid::parse_str(&record.input_id)
        .ok()
        .is_some_and(|id| ids.contains(&MessageId::from(id)))
}

pub(super) fn ready_ids(state: &MailboxState) -> Vec<String> {
    state.ready.clone()
}

pub(super) fn promote_next(state: &mut MailboxState) -> bool {
    if !state.ready.is_empty()
        || state.records.iter().any(|record| {
            matches!(
                record.state,
                UserInputState::Dispatching | UserInputState::Claimed
            )
        })
    {
        return false;
    }
    let next = state
        .records
        .iter()
        .find(|record| record.state == UserInputState::Queued)
        .map(|record| record.input_id.clone());
    next.is_some_and(|id| !promote_ids(state, &[id]).is_empty())
}

pub(super) fn promote_ids(state: &mut MailboxState, ids: &[String]) -> Vec<String> {
    let mut promoted = Vec::new();
    // 单次 ALL 按服务端原顺序；后续命令追加到 ready，保持 B 后 A 的接受顺序。
    for record in &mut state.records {
        if record.state == UserInputState::Queued && ids.contains(&record.input_id) {
            record.state = UserInputState::Dispatching;
            record.handed_off = false;
            state.ready.push(record.input_id.clone());
            promoted.push(record.input_id.clone());
        }
    }
    if !promoted.is_empty() {
        state.revision += 1;
    }
    promoted
}

pub(super) fn user_input_payload(entry: &MqEntry) -> Option<(MessageContent, String)> {
    match &entry.payload {
        PersistedPayload::Message(BaseMessage::Human { content, .. }) => Some((
            content.clone(),
            entry.original_draft.clone().unwrap_or_default(),
        )),
        _ => None,
    }
}

pub(super) fn user_input_from_entry(entry: &MqEntry) -> Option<UserInput> {
    let (content, original_draft) = user_input_payload(entry)?;
    Some(UserInput {
        input_id: entry.id.as_uuid().to_string(),
        content,
        original_draft,
    })
}

pub(super) fn fingerprint_for(
    input_id: &str,
    content: &MessageContent,
    original_draft: &str,
) -> u64 {
    compute_fingerprint(("user_input", input_id, content, original_draft))
}

pub(super) fn compute_fingerprint<T: serde::Serialize>(value: T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&value)
        .expect("队列 DTO 可序列化")
        .hash(&mut hasher);
    hasher.finish()
}
