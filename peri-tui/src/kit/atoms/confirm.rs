//! 通用确认弹窗的版本身份、动作与状态。

use ratatui_kit::prelude::Atom as AtomStatic;

/// 确认弹窗要执行的操作
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// 授权仅属于弹窗展示时的来源会话，不能在点击时重新采样。
    ThreadSwitch {
        target_id: String,
        source_identity: Option<(String, u64)>,
    },
    /// 用户确认拒绝回答 AskUser 提问
    RejectAskUser {
        owner: crate::acp_client::InteractionOwner,
        request_id_json: String,
    },
}

/// 确认弹窗的 payload
#[derive(Debug, Clone)]
pub struct ConfirmPayload {
    /// 进程内单调递增的版本——payload 的业务身份，由 [`ConfirmPayload::new`] 分配。
    pub version: u64,
    pub title: String,
    pub message: String,
    pub details: Vec<String>,
    pub pending_action: ConfirmAction,
}

/// payload 版本计数器（进程内单调递增）。
static CONFIRM_PAYLOAD_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ConfirmPayload {
    /// 构造确认 payload——版本由本函数分配，发布点不得自带版本号。
    ///
    /// [Why] 弹窗渲染的是某一帧读到的 payload，确认键却在按下的瞬间重新读取全局
    /// payload；两者之间 payload 可能被其它发布点替换。版本随 payload 一起发布，
    /// 弹窗据此拒绝执行「用户没看到的动作」。
    pub fn new(
        title: String,
        message: String,
        details: Vec<String>,
        pending_action: ConfirmAction,
    ) -> Self {
        let version =
            CONFIRM_PAYLOAD_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        Self {
            version,
            title,
            message,
            details,
            pending_action,
        }
    }
}

pub static CONFIRM_PAYLOAD: AtomStatic<Option<ConfirmPayload>> = AtomStatic::new(|| None);
