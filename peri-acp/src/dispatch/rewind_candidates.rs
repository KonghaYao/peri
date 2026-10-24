//! `session/rewind-candidates` dispatch handler.
//!
//! 查询回退候选：存储按 `role = 'user'` 有界读取（最新在前、最多
//! [`REWIND_CANDIDATE_LIMIT`] 条），本层只做展示投影——剥离
//! `<system-reminder>` 注入块、截断 200 字符。返回 `{ messages: [{ id, preview }] }`，
//! id 为服务端权威 `MessageId`。
//!
//! 注意：Bypass 模式下首轮 user 消息末尾会被追加 `<system-reminder>` 权限通知，
//! 不能直接 `contains` 过滤，否则会连带排除用户真实输入（rewind 候选为空）。
//! preview 生成时经 `strip_system_reminders` 剔除注入块，只保留用户真实文本
//! （TUI 回填/弹窗展示的都是干净的用户输入）。
//!
//! 候选资格（Human、纯注入排除）由 `peri-acp-types` 的共享规则判定，本层不重复实现；
//! `.take(REWIND_CANDIDATE_LIMIT)` 仍保留为 capability boundary：存储返回超界时
//! 只读选择器也不能变成无界响应。

use peri_acp_types::messages::{strip_system_reminders, BaseMessage};
use peri_acp_types::session_resources::{rewind_candidate_text, RewindCandidate};
use serde_json::{json, Value};

use crate::transport::types::AcpError;

/// 一次候选查询最多返回的条数（存储与 ACP 展示层共用同一上限）。
pub const REWIND_CANDIDATE_LIMIT: usize = 64;

/// 候选的 wire 投影：只保留 `{ id, preview }`。
///
/// 顺序由存储保证（最新在前）；本层不重排、不去重。返回文本为空（例如全是注入块）
/// 的候选丢弃，且最多输出 [`REWIND_CANDIDATE_LIMIT`] 条。
pub fn rewind_candidate_response(candidates: &[RewindCandidate]) -> Value {
    let messages: Vec<Value> = candidates
        .iter()
        .filter_map(|candidate| {
            // 剥离 system-reminder 注入块：Bypass 权限通知/recall 等不应进入
            // 候选预览与输入框回填。
            let preview = strip_system_reminders(&candidate.text);
            let preview = preview.trim();
            (!preview.is_empty()).then(|| {
                json!({
                    "id": candidate.message_id.as_uuid().to_string(),
                    "preview": preview.chars().take(200).collect::<String>(),
                })
            })
        })
        .take(REWIND_CANDIDATE_LIMIT)
        .collect();

    json!({ "messages": messages })
}

/// 从 `BaseMessage` 历史提取回退候选（纯计算，无副作用；只供既有测试与内联调用）。
///
/// 生产路径不再经过这里：handler 直接用存储的有界读取结果。
pub fn rewind_candidates(session_history: &[BaseMessage]) -> Result<Value, AcpError> {
    let candidates: Vec<RewindCandidate> = session_history
        .iter()
        .rev() // 最新在前——弹窗第一条 = 最近一次 user 消息 = 回退一步
        .filter_map(|message| {
            rewind_candidate_text(message).map(|text| RewindCandidate {
                message_id: message.id(),
                text,
            })
        })
        .take(REWIND_CANDIDATE_LIMIT)
        .collect();
    Ok(rewind_candidate_response(&candidates))
}

#[cfg(test)]
#[path = "rewind_candidates_test.rs"]
mod tests;
