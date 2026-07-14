//! Hook DTOs -- 取代 peri_middlewares::hooks::types::{HookEvent, HookType, RegisteredHook}

use serde::{Deserialize, Serialize};

/// 注册的 Hook（对齐 peri_middlewares::hooks::types::RegisteredHook）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredHookDto {
    pub id: String,
    pub event: HookEventDto,
    pub hook_type: HookTypeDto,
    pub enabled: bool,
}

/// Hook 触发事件（对齐 peri_middlewares::hooks::types::HookEvent）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookEventDto {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    Stop,
    SessionStart,
}

/// Hook 执行类型（对齐 peri_middlewares::hooks::types::HookType）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookTypeDto {
    Command { cmd: String },
    Prompt { prompt: String },
}
