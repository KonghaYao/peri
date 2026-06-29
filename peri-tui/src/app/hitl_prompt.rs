use peri_middlewares::prelude::{BatchItem, HitlDecision}; // P4b: type-dependency, oneshot channels

// ─── PendingAttachment ────────────────────────────────────────────────────────

/// 待发送的图片附件（Ctrl+V 从剪贴板粘贴）
pub struct PendingAttachment {
    /// 显示名称，如 "clipboard_1.png"
    pub label: String,
    /// MIME 类型，固定为 "image/png"
    pub media_type: String,
    /// base64 编码的 PNG 数据
    pub base64_data: String,
    /// PNG 文件大小（字节，用于显示）
    pub size_bytes: usize,
}

// ─── HitlBatchPrompt ──────────────────────────────────────────────────────────

/// 批量 HITL 弹窗状态：每项独立的批准/拒绝选择
pub struct HitlBatchPrompt {
    /// 待审批的工具调用列表
    pub items: Vec<BatchItem>,
    /// 每项的当前决策（true=批准，false=拒绝）
    pub approved: Vec<bool>,
    /// 当前光标所在的行（工具索引）
    pub cursor: usize,
    /// 回复 channel
    pub response_tx: tokio::sync::oneshot::Sender<Vec<HitlDecision>>,
}

impl HitlBatchPrompt {
    pub fn new(
        items: Vec<BatchItem>,
        response_tx: tokio::sync::oneshot::Sender<Vec<HitlDecision>>,
    ) -> Self {
        let len = items.len();
        Self {
            items,
            approved: vec![true; len], // 默认全部批准
            cursor: 0,
            response_tx,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        self.cursor = ((self.cursor as isize + delta).rem_euclid(len as isize)) as usize;
    }

    /// 切换当前项的批准/拒绝状态
    pub fn toggle_current(&mut self) {
        if let Some(v) = self.approved.get_mut(self.cursor) {
            *v = !*v;
        }
    }

    /// 全部批准
    pub fn approve_all(&mut self) {
        self.approved.iter_mut().for_each(|v| *v = true);
    }

    /// 全部拒绝
    pub fn reject_all(&mut self) {
        self.approved.iter_mut().for_each(|v| *v = false);
    }

    /// 确认并发送决策
    pub fn confirm(self) {
        let decisions: Vec<HitlDecision> = self
            .approved
            .iter()
            .map(|&ok| {
                if ok {
                    HitlDecision::Approve
                } else {
                    HitlDecision::Reject
                }
            })
            .collect();
        let _ = self.response_tx.send(decisions);
    }
}
