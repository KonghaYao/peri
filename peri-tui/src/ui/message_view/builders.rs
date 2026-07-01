//! MessageViewModel 工厂函数。

use crate::ui::markdown::parse_markdown_default;
use crate::ui::theme;

use super::MessageViewModel;
use super::tool_color;

impl MessageViewModel {
    /// 创建用户消息
    pub fn user(content: String) -> Self {
        let rendered = parse_markdown_default(&content);
        let mut vm = MessageViewModel::UserBubble {
            content,
            rendered,
            content_hash: 0,
            system_reminder: false,
        };
        vm.recompute_hash();
        vm
    }

    /// 创建助手消息
    pub fn assistant() -> Self {
        let mut vm = MessageViewModel::AssistantBubble {
            blocks: Vec::new(),
            is_streaming: true,
            collapsed: false,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }

    /// 创建工具消息
    pub fn tool_block(
        tool_name: String,
        display: String,
        args: Option<String>,
        is_error: bool,
    ) -> Self {
        Self::tool_block_with_id(String::new(), tool_name, display, args, is_error)
    }

    /// 创建带 tool_call_id 的工具消息（SubAgent 内部并行工具调用精确匹配）
    pub fn tool_block_with_id(
        tool_call_id: String,
        tool_name: String,
        display: String,
        args: Option<String>,
        is_error: bool,
    ) -> Self {
        let color = if is_error {
            theme::ERROR
        } else {
            tool_color(&tool_name)
        };
        let mut vm = MessageViewModel::ToolBlock {
            tool_call_id,
            tool_name,
            display_name: display,
            args_display: args,
            content: String::new(),
            is_error,
            collapsed: true,
            color,
            diff_lines: None,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }

    /// 创建系统消息
    pub fn system(content: String) -> Self {
        let mut vm = MessageViewModel::SystemNote {
            content,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }

    /// 创建缓存率警告消息（黄色纯文本，无前缀符号）
    pub fn cache_warning(content: String) -> Self {
        let mut vm = MessageViewModel::CacheWarning {
            content,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }

    /// 创建 SubAgentGroup（初始状态：运行中、展开、0 步）
    pub fn subagent_group(agent_id: String, task_preview: String) -> Self {
        let mut vm = MessageViewModel::SubAgentGroup {
            agent_id,
            task_preview,
            total_steps: 0,
            recent_messages: Vec::new(),
            is_running: true,
            collapsed: false,
            final_result: None,
            is_error: false,
            is_background: false,
            bg_hash: None,
            batch_agents: Vec::new(),
            instance_id: None,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }
}
