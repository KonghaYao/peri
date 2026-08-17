use super::fold::{FoldState, fold_state_code};
use super::hash::{tui_hash_combine, tui_hash_str};

/// AskUser question-answer block — rendered after user responds to AskUserQuestion tool.
///
/// Slice 4（§6.8）双轨落地：production 创建点（`handle_ask_user` /
/// `handle_hitl_pending`）push 到 `state.committed`（不进 CurrentTurn 缓存），
/// 承担「可见 + 可聚焦 + 结果回写」；AskUser 面板 / HITL 弹窗保留为模态操作层
/// （D5）。历史 items 字段保留（问答对渲染兼容旧数据），新路径以
/// kind/pending/verb/question/options/result 为准。
#[derive(Debug, Clone)]
pub struct TuiAskUserBlock {
    /// Question-answer pairs extracted from tool input/output（历史字段）。
    pub items: Vec<TuiAskUserItem>,
    /// Whether any item indicates an error response.
    pub is_error: bool,
    /// 交互类型：Permission（HITL）或 AskUser 表单（§6.8）。
    pub kind: InteractionKind,
    /// 是否仍在等待用户响应。pending → 折叠表 Running（Expanded 可聚焦）；
    /// 结果回写后 false → Completed（Expanded 完整展示，不自动收束）。
    pub pending: bool,
    /// 动作动词（如 `Bash`；AskUser 恒 `AskUser`）。
    pub verb: String,
    /// 人类可读摘要（Permission：`Bash wants to run: cargo test`；
    /// AskUser：首问 header/options 摘要）。
    pub question: String,
    /// 可选项 label 列表（Permission：[Allow once, Deny]，D6 协议依赖；
    /// AskUser：首问 options labels）。
    pub options: Vec<String>,
    /// 提交结果（如 `Allowed once` / 用户选中 label）——仅 completed 有值；
    /// 渲染层负责加状态符号与颜色。
    pub result: Option<String>,
    /// 本地 request_id（从 HITL_REQUEST_ID / ASK_USER_REQUEST_ID atom 克隆，
    /// 即 serde_json 序列化的 RequestId 字符串）——InteractionResolved 事件
    /// 按此匹配回写；同时是折叠覆盖键 `FoldKey::Interaction(id)` 的键控。
    /// 身份字段，不进 content_hash（同 message_id/source 先例），进 partial_eq。
    pub request_id: Option<String>,
    /// 折叠状态——折叠 pass（spec §7 interaction 行）驱动；
    /// 生产创建点 push 到 committed，折叠 pass 与用户覆盖共同驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

/// §6.8 interaction block 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionKind {
    /// HITL RequestPermission 审批（`[Allow once] [Deny]`）。
    Permission,
    /// AskUser 表单（选项取首问 options labels）。
    AskUser,
}

/// [G1] InteractionKind 对 hash 的确定性贡献。
pub fn interaction_kind_code(k: &InteractionKind) -> u64 {
    match k {
        InteractionKind::Permission => 1,
        InteractionKind::AskUser => 2,
    }
}

impl TuiAskUserBlock {
    /// [G1] 内容哈希公式单点——生产创建点 / 结果回写 / 折叠 pass 共用。
    /// 包含 kind/pending/verb/question/options/result + fold/is_error/user_modified；
    /// `request_id` 是身份字段不参与（同 message_id 先例）。result 秒级稳定
    /// （提交后定型），pending 翻转与选项变化必须触发按 hash 分片的缓存重建。
    pub fn recompute_hash(&mut self) {
        let mut h = tui_hash_combine(0, interaction_kind_code(&self.kind));
        h = tui_hash_combine(h, u64::from(self.pending));
        h = tui_hash_combine(h, tui_hash_str(&self.verb));
        h = tui_hash_combine(h, tui_hash_str(&self.question));
        for opt in &self.options {
            h = tui_hash_combine(h, tui_hash_str(opt));
        }
        h = tui_hash_combine(h, self.options.len() as u64);
        h = tui_hash_combine(
            h,
            match &self.result {
                Some(r) => tui_hash_str(r),
                None => 0,
            },
        );
        h = tui_hash_combine(h, fold_state_code(self.fold));
        h = tui_hash_combine(h, u64::from(self.is_error));
        h = tui_hash_combine(h, u64::from(self.user_modified));
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiAskUserBlock: items, is_error, kind, pending, verb, question, options, result, request_id, fold, user_modified);

/// A single question-answer pair in an AskUser block.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiAskUserItem {
    /// Question header text.
    pub header: String,
    /// User's answer text.
    pub answer: String,
}
