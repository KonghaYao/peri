//! HookInput 构造工厂。
//!
//! 集中原本散落在 middleware / standalone 路径中 4 处直接结构体字面量构造的
//! `HookInput`，统一 session_id / transcript_path / cwd / permission_mode 字段
//! 一致性。原 `types.rs` 中已存在的构造函数（`session_start` / `user_prompt_submit`
//! / `tool_call` / `tool_result` / `compact`）保持不变，本模块仅补充
//! PostToolBatch / Stop / StopFailure / SessionEnd / Notification 等
//! "无专用构造函数"的场景。
//!
//! [TRAP] 所有字面量构造必须通过这里，禁止在新代码里再次复制
//! `session_id: ..., transcript_path: ..., cwd: ..., permission_mode: Some(...)`
//! 模板——历史 bug 多源于此（漏字段 / 字段错位）。

use peri_agent::agent::react::AgentOutput;

use crate::hooks::types::{HookEvent, HookInput};

/// 构造 PostToolBatch 的 `HookInput`，从 state 视角聚合 prompt + message_count。
pub fn post_tool_batch(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    permission_mode_str: &str,
    current_model: &str,
    prompt: &str,
    message_count: usize,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: Some(permission_mode_str.to_string()),
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::PostToolBatch,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: Some(prompt.to_string()),
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: Some(message_count),
        additional_data: None,
    }
}

/// 构造 Stop hook 的 `HookInput`。
///
/// subagent_result 携带 agent 最终输出（截断到 500 字符），
/// source 携带 stop_reason（若存在）标识结束原因。
pub fn stop(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    permission_mode_str: &str,
    current_model: &str,
    output: &AgentOutput,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: Some(permission_mode_str.to_string()),
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::Stop,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: output
            .stop_reason
            .as_deref()
            .map(|_| "agent_complete".to_string()),
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: Some(output.text.chars().take(500).collect::<String>()),
        message_count: None,
        additional_data: None,
    }
}

/// 构造 StopFailure hook 的 `HookInput`，`tool_output` 携带 `{:?}` 错误描述。
pub fn stop_failure(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    permission_mode_str: &str,
    current_model: &str,
    error_description: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: Some(permission_mode_str.to_string()),
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::StopFailure,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: Some(serde_json::json!(error_description)),
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 SessionEnd 的 `HookInput`。
pub fn session_end_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
    reason: Option<&str>,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::SessionEnd,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: reason.map(|r| r.to_string()),
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 Notification 的 `HookInput`。
pub fn notification_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::Notification,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

// === P1-5 新增 standalone 构造函数 ===

/// 构造 standalone 路径 Setup 的 `HookInput`，session 初始化时触发。
pub fn setup_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::Setup,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 InstructionsLoaded 的 `HookInput`，
/// 每次 user prompt 时规则/skill 已加载。
pub fn instructions_loaded_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::InstructionsLoaded,
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 ConfigChange 的 `HookInput`。
pub fn config_change_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
    config_key: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::ConfigChange,
        tool_name: Some(config_key.to_string()),
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 WorktreeCreate 的 `HookInput`。
pub fn worktree_create_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
    worktree_path: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::WorktreeCreate,
        tool_name: Some(worktree_path.to_string()),
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 WorktreeRemove 的 `HookInput`。
pub fn worktree_remove_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
    worktree_path: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::WorktreeRemove,
        tool_name: Some(worktree_path.to_string()),
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}

/// 构造 standalone 路径 CwdChanged 的 `HookInput`。
/// old_cwd 记录变更前的工作目录。
pub fn cwd_changed_standalone(
    session_id: &str,
    transcript_path: &str,
    cwd: &str,
    current_model: &str,
    old_cwd: &str,
) -> HookInput {
    HookInput {
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        cwd: cwd.to_string(),
        permission_mode: None,
        agent_id: None,
        agent_type: None,
        hook_event_name: HookEvent::CwdChanged,
        tool_name: Some(old_cwd.to_string()),
        tool_input: None,
        tool_use_id: None,
        tool_output: None,
        prompt: None,
        source: None,
        model: Some(current_model.to_string()),
        subagent_name: None,
        subagent_result: None,
        message_count: None,
        additional_data: None,
    }
}
