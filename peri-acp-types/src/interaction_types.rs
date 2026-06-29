//! HITL + AskUser DTOs -- 取代 peri_middlewares::{hitl::*, ask_user::*}

use serde::{Deserialize, Serialize};

/// HITL 批量审批项（对齐 peri_middlewares::hitl::BatchItem）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchItemDto {
    pub tool_name: String,
    pub input_summary: String,
    pub tool_call_id: String,
}

/// HITL 审批决策（对齐 peri_middlewares::prelude::HitlDecision）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HitlDecisionDto {
    Accept,
    Reject,
    AcceptAll,
}

/// AskUser 问题数据（对齐 peri_middlewares::ask_user::AskUserQuestionData）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserQuestionDataDto {
    pub tool_call_id: String,
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    pub options: Vec<AskUserOptionDto>,
}

/// AskUser 选项（对齐 peri_middlewares::ask_user::AskUserOption）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserOptionDto {
    pub label: String,
    pub description: Option<String>,
}

/// AskUser 批量请求（对齐 peri_middlewares::ask_user::AskUserBatchRequest）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserBatchRequestDto {
    pub questions: Vec<AskUserQuestionDataDto>,
}

/// Thread 元数据（对齐 peri_agent::thread::ThreadMeta）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadMetaDto {
    pub thread_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Token 使用记录（对齐 peri_agent::agent::token::RequestRecord）
/// 用于 StatusPanel 显示。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestRecordDto {
    pub request_id: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
    pub model: Option<String>,
    pub timestamp: i64,
}

/// 共享权限模式包装（用于 ServiceRegistry）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedPermissionModeDto {
    pub mode: PermissionModeDto,
}

use crate::permission::PermissionModeDto;
