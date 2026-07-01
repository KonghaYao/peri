/// 会话元数据：低频访问的会话状态。
///
/// (S13c-4c) `pending_attachments` 字段保留为 Vec<String>——legacy
/// `PendingAttachment` 结构已删除，但 ACP server 侧上传协议仍可能用到
/// 附件路径列表（保留 minimal 形式以兼容未来扩展）。
pub struct SessionMetadata {
    pub session_id: uuid::Uuid,
    pub pending_attachments: Vec<String>,
    pub last_human_message: Option<String>,
    pub pre_submit_state_len: usize,
}

impl SessionMetadata {
    pub fn new() -> Self {
        Self {
            session_id: uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)),
            pending_attachments: Vec::new(),
            last_human_message: None,
            pre_submit_state_len: 0,
        }
    }
}

impl Default for SessionMetadata {
    fn default() -> Self {
        Self::new()
    }
}
