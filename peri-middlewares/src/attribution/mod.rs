//! Git attribution 中间件。
//!
//! 追踪 Write/Edit 工具修改的文件，累积贡献字符数。
//! Co-Authored-By 指令在 system prompt 构建时注入（`build_bare_agent`）。
//!
//! ## 钩子流程
//!
//! ```text
//! before_tool (Write/Edit) → 读取旧文件内容 → 存入 pending
//!   → [工具执行]
//! after_tool  (Write/Edit) → 读取新文件内容 → track_change()
//! ```

mod model_email;
mod state;

use peri_agent::middleware::capabilities as hook_state;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
pub use model_email::get_attribution_email;
use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    error::AgentResult,
    middleware::r#trait::Middleware,
};
pub use state::AttributionState;

use crate::tool_search::core_tools::{TOOL_EDIT, TOOL_WRITE};

/// Git 留名中间件
///
/// 注册在 `ChainSlot::GitAttribution`（第二组；v4-part-4 W3-C1 后本组已不含文件/终端
/// 工具提供器，二者由 builtin `workspace` 实例提供；v4 wave 4 后 GitWatch 已从链上删除，
/// 本中间件是组内唯一成员），hook 其 Write/Edit 工具调用。
/// `before_tool` 暂存旧文件内容，`after_tool` 计算贡献字符数。
/// Co-Authored-By 指令由 `build_bare_agent` 在 system prompt 中注入。
pub struct GitAttributionMiddleware {
    reader: Arc<dyn crate::workspace_io::WorkspaceFileReader>,
    state: Arc<Mutex<AttributionState>>,
    pending_old_content: Arc<Mutex<HashMap<String, String>>>,
    branch_baseline: Arc<Mutex<Option<String>>>,
    /// Cached prompt contribution text.
    attribution_text: String,
}

impl GitAttributionMiddleware {
    pub fn new(
        model_name: &str,
        reader: Arc<dyn crate::workspace_io::WorkspaceFileReader>,
    ) -> Self {
        let attribution_text = Self::attribution_text(model_name);
        Self {
            reader,
            state: Arc::new(Mutex::new(AttributionState::new(model_name.to_string()))),
            pending_old_content: Arc::new(Mutex::new(HashMap::new())),
            branch_baseline: Arc::new(Mutex::new(None)),
            attribution_text,
        }
    }

    /// 生成 attribution 文本（静态方法，供 system prompt 构建使用）。
    pub fn attribution_text(model_name: &str) -> String {
        AttributionState::new(model_name.to_string()).co_authored_by()
    }

    /// 获取当前 attribution text��用于调试）
    pub fn current_attribution_text(&self) -> String {
        self.state.lock().unwrap().co_authored_by()
    }

    /// Clear per-turn state for reuse across prompts.
    pub fn reset(&self) {
        self.pending_old_content.lock().unwrap().clear();
    }

    fn observe_branch(&self, current: String) -> Option<(String, String)> {
        let mut baseline = self.branch_baseline.lock().unwrap();
        match baseline.replace(current.clone()) {
            Some(previous) if previous != current => Some((previous, current)),
            _ => None,
        }
    }
}

#[async_trait]
impl Middleware for GitAttributionMiddleware {
    fn name(&self) -> &str {
        "GitAttributionMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        let text = format!(
            "\n\n## Git Attribution\n\nWhen the user asks you to commit, append the following line to the commit message:\n\n```\n{}\n```\n\nThis tracks AI contributions for code you authored. Only include it when you are already creating a commit at the user's request.",
            self.attribution_text
        );
        Some(text)
    }

    async fn before_tool(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        // 仅处理 Write 和 Edit
        if tool_call.name != TOOL_WRITE && tool_call.name != TOOL_EDIT {
            return Ok(tool_call.clone());
        }
        // 读取当前文件内容，暂存到 pending
        if let Some(file_path) = tool_call.input.get("file_path").and_then(|v| v.as_str()) {
            let path = std::path::Path::new(state.cwd()).join(file_path);
            self.pending_old_content
                .lock()
                .unwrap()
                .remove(&tool_call.id);
            if let Ok(old_content) = self.reader.read_text(&path).await {
                self.pending_old_content
                    .lock()
                    .unwrap()
                    .insert(tool_call.id.clone(), old_content);
            }
        }
        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        state: &mut dyn hook_state::AfterToolState,
        tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        // 仅处理 Write 和 Edit
        if tool_call.name != TOOL_WRITE && tool_call.name != TOOL_EDIT {
            return Ok(());
        }
        let file_path = match tool_call.input.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return Ok(()),
        };
        let old_content = self
            .pending_old_content
            .lock()
            .unwrap()
            .remove(&tool_call.id)
            .unwrap_or_default();
        let path = std::path::Path::new(state.cwd()).join(file_path);
        let new_content = match self.reader.read_text(&path).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        self.state
            .lock()
            .unwrap()
            .track_change(file_path, &old_content, &new_content);
        Ok(())
    }

    async fn before_agent(&self, _state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        if let Ok(Some(branch)) = self.reader.current_branch().await {
            if let Some((previous_branch, current_branch)) = self.observe_branch(branch) {
                tracing::info!(
                    target: "git",
                    previous_branch,
                    current_branch,
                    "Git branch changed during the session"
                );
            }
        }
        // Attribution 指令已在 system prompt 中注入，无需再向消息历史写入。
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "io_test.rs"]
mod io_tests;
