//! transcript 测试挂载点与共享夹具。
//!
//! 用例按接口分组拆到子模块：`persistence_tests`（异步持久化行为，含真实 SQLite
//! 夹具）、`memory_tests`（内存模型：构造/寻址/staging/标记与投影/祖先/rewind/
//! rebuild）、`reminder_tests`（canonical reminder 的受众投递矩阵）。

use super::*;

use crate::messages::MessageContent;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text(text.to_string()),
    )
}

#[path = "transcript_persistence_test.rs"]
mod persistence_tests;

#[path = "transcript_memory_test.rs"]
mod memory_tests;

#[path = "transcript_memory_probe_test.rs"]
mod memory_probe_tests;

#[path = "p_matrix_test.rs"]
mod p_matrix_tests;

#[path = "transcript_reminder_test.rs"]
mod reminder_tests;
