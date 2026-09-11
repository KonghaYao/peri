//! Tolerant invocation decoding and live parent-message selection.
use peri_agent::messages::BaseMessage;

pub(super) struct InvocationArgs {
    pub(super) resume_thread_id: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) subagent_type: Option<String>,
    pub(super) model: Option<String>,
    pub(super) run_in_background: bool,
    pub(super) cwd: String,
    pub(super) is_fork: bool,
}

impl InvocationArgs {
    pub(super) fn parse(input: &serde_json::Value, parent_cwd: &str) -> Self {
        // resume_thread_id 仅在值为有效 UUID 时才视为恢复意图（「不填 = 新建」语义）：
        // LLM 表达「省略」时常用 "" / "new" / "__omit__" 等占位符（或把意图填进
        // 该字段），若按 is_some 判断会被劫持进 resume 分支并触发 invalid thread id
        // 失败——占位符一律忽略，走正常新建路径（subagent_type / fork / prompt）。
        // 真实 child_thread_id 恒为 UUID（spawn 时 Uuid::now_v7 生成），过滤不损失语义。
        let resume_thread_id = input
            .get("resume_thread_id")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && uuid::Uuid::parse_str(s).is_ok())
            .map(|s| s.to_string());
        // prompt 改为 Option：resume 路径可缺省（缺省注入隐式 continue，issue 决策 9）；
        // 非 resume 路径下方运行时校验兜底（required:[] 后语义不变）
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let subagent_type = input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // model 档位覆盖（仅新建定义型 subagent 消费；fork/resume 路径忽略，
        // 与 resume 忽略 subagent_type/fork 的宽容语义一致）
        let model = input
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let run_in_background = input
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cwd = input
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or(parent_cwd)
            .to_string();
        let is_fork = input.get("fork").and_then(|v| v.as_bool()).unwrap_or(false)
            || subagent_type.as_deref() == Some("fork");

        Self {
            resume_thread_id,
            prompt,
            subagent_type,
            model,
            run_in_background,
            cwd,
            is_fork,
        }
    }
}

impl super::SubAgentTool {
    /// Prefer the current ToolContext over the before_agent snapshot. Remove only
    /// the trailing AI tool call whose ToolResult has not arrived yet.
    pub(super) fn current_messages(&self, live_messages: &[BaseMessage]) -> Vec<BaseMessage> {
        let mut msgs: Vec<peri_agent::messages::BaseMessage> = if !live_messages.is_empty() {
            live_messages.to_vec()
        } else if let Some(ref pm) = self.parent_messages {
            pm.read().clone()
        } else {
            Vec::new()
        };
        if let Some(last) = msgs.last() {
            if last.has_tool_calls() {
                msgs.pop();
            }
        }
        msgs
    }
}
