use super::{
    async_trait, hook_state, AgentResult, Arc, AttributionState, HashMap, Middleware, Mutex,
    ToolCall, ToolResult, TOOL_EDIT, TOOL_WRITE,
};
use std::fmt;
use std::fmt::{Debug, Formatter};

/// Git 留名中间件
///
/// 注册在 `ChainSlot::GitAttribution`（第二组；v4-part-4 W3-C1 后本组已不含文件/终端
/// 工具提供器，二者由 builtin `workspace` 实例提供；v4 wave 4 后 GitWatch 已从链上删除，
/// 本中间件是组内唯一成员），hook 其 Write/Edit 工具调用。
/// `before_tool` 暂存旧文件内容，`after_tool` 计算贡献字符数。
/// Co-Authored-By 指令由 `build_bare_agent` 在 system prompt 中注入。
pub struct GitAttributionMiddleware {
    pub(super) reader: Arc<dyn crate::workspace_io::WorkspaceFileReader>,
    pub(super) state: Arc<Mutex<AttributionState>>,
    pub(super) pending_old_content: Arc<Mutex<HashMap<String, String>>>,
    pub(super) branch_baseline: Arc<Mutex<Option<String>>>,
    /// Cached prompt contribution text.
    pub(super) attribution_text: String,
}

impl Debug for GitAttributionMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitAttributionMiddleware")
            .field("attribution_text", &self.attribution_text)
            .finish_non_exhaustive()
    }
}

impl GitAttributionMiddleware {
    #[must_use]
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
    #[must_use]
    pub fn attribution_text(model_name: &str) -> String {
        AttributionState::new(model_name.to_string()).co_authored_by()
    }

    /// 获取当前 attribution text��用于调试）
    #[must_use]
    pub fn current_attribution_text(&self) -> String {
        self.state
            .lock()
            .expect("Git attribution state must not contain a partially applied contribution")
            .co_authored_by()
    }

    /// Clear per-turn state for reuse across prompts.
    pub fn reset(&self) {
        self.pending_old_content
            .lock()
            .expect("Git attribution baseline must remain consistent across tool execution")
            .clear();
    }

    pub(super) fn observe_branch(&self, current: String) -> Option<(String, String)> {
        let mut baseline = self
            .branch_baseline
            .lock()
            .expect("Git attribution branch baseline must remain consistent after an update");
        match baseline.replace(current.clone()) {
            Some(previous) if previous != current => Some((previous, current)),
            _ => None,
        }
    }
}

#[async_trait]
impl Middleware for GitAttributionMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::GitAttribution.name()
    }

    fn prompt_contribution(&self) -> Option<String> {
        // M1：分隔符由 `MiddlewareChain::collect_prompt_contributions` 统一
        // 负责（非空贡献以空行连接）——贡献正文不再自带前导 `\n\n`。
        let text = format!(
            "## Git Attribution\n\nWhen the user asks you to commit, append the following line to the commit message:\n\n```\n{}\n```\n\nThis tracks AI contributions for code you authored. Only include it when you are already creating a commit at the user's request.",
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
                .expect("Git attribution baseline must remain consistent across tool execution")
                .remove(&tool_call.id);
            if let Ok(old_content) = self.reader.read_text(&path).await {
                self.pending_old_content
                    .lock()
                    .expect("Git attribution baseline must remain consistent across tool execution")
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
            .expect("Git attribution baseline must remain consistent across tool execution")
            .remove(&tool_call.id)
            .unwrap_or_default();
        let path = std::path::Path::new(state.cwd()).join(file_path);
        let new_content = match self.reader.read_text(&path).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        self.state
            .lock()
            .expect("Git attribution state must not contain a partially applied contribution")
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
