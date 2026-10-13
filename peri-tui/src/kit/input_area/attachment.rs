//! 输入区附件的整张移除语义。
//!
//! 粘贴图片走上传式附件（`PENDING_ATTACHMENTS`，见 [`super::paste`]），与编辑器文本
//! 分开存放——编辑器里没有对应字符，因此「整张删除」需要一个显式的边缘语义：
//! **文本为空时，一次删除键移除一张附件**（不逐字符删）。
//!
//! 不引入占位字符的好处：折行宽度、@mention 检测、选区、undo 与提交内容形状都保持
//! 原样（附件记录只在呈现层与提交载荷中出现）。

use crate::components::textarea::TextAreaState;
use crate::kit::atoms::PENDING_ATTACHMENTS;

/// 文本为空时删除键对附件的去处。
#[derive(Clone, Copy)]
pub(super) enum AttachmentEdge {
    /// Backspace：移除尾部一张（后粘贴的先移除）。
    Last,
    /// Delete：移除头部一张（与文本流「向前删」同向的对称语义）。
    First,
}

/// 编辑区文本为空时，一次删除键移除**整张**图片附件（一次一张，不逐字符删）。
///
/// 返回 `true` = 本次按键已消费为「移除附件」，调用方不再改文本。文本非空时一律
/// 不处理——逐字符删除、删词、选区、undo 等既有编辑语义完全不变。
pub(super) fn remove_pending_attachment(editor: &TextAreaState, edge: AttachmentEdge) -> bool {
    if !editor.text.is_empty() {
        return false;
    }
    let state = PENDING_ATTACHMENTS.state();
    let mut pending = state.write();
    let len = pending.len();
    if len == 0 {
        return false;
    }
    match edge {
        AttachmentEdge::Last => {
            pending.remove(len - 1);
        }
        AttachmentEdge::First => {
            pending.remove(0);
        }
    }
    true
}

#[cfg(test)]
#[path = "attachment_test.rs"]
mod tests;
