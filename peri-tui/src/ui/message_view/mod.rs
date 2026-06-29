use std::hash::{Hash, Hasher};

use ratatui::{
    style::Color,
    text::{Line, Text},
};

mod aggregate;
mod build;
mod builders;
mod tools;
mod utils;

pub use aggregate::{aggregate_batch_groups, aggregate_tail_tool_groups, aggregate_tool_groups};
pub use tools::{tool_color, AgentSummary, ToolCategory, ToolEntry};

/// 渲染层的视图模型，从 BaseMessage/AgentEvent 转换而来
#[derive(Debug, Clone)]
pub enum MessageViewModel {
    /// 用户输入
    UserBubble {
        #[allow(dead_code)]
        content: String,
        rendered: Text<'static>,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
        /// 是否为 <system-reminder> 包裹的系统提醒消息（compact summary 等）
        system_reminder: bool,
    },
    /// AI 回复（支持流式追加）
    AssistantBubble {
        blocks: Vec<ContentBlockView>,
        is_streaming: bool,
        /// 折叠状态：true 表示完全隐藏，false 表示展开显示
        collapsed: bool,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
    /// 工具调用结果
    ToolBlock {
        #[allow(dead_code)]
        tool_name: String,
        tool_call_id: String,
        display_name: String,
        args_display: Option<String>,
        content: String,
        is_error: bool,
        collapsed: bool,
        color: Color,
        /// 内嵌 diff 视图（Write/Edit 工具执行成功后填充，预渲染缓存）
        diff_lines: Option<Vec<Line<'static>>>,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
    /// 系统消息
    SystemNote {
        content: String,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
    /// 缓存率过低警告（黄色纯文本，无前缀符号）
    CacheWarning {
        content: String,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
    /// 只读工具调用聚合组（read/search/glob 折叠显示）
    ToolCallGroup {
        category: ToolCategory,
        tools: Vec<ToolEntry>,
        collapsed: bool,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
    /// SubAgent 执行块（可折叠，含滑动窗口消息）
    SubAgentGroup {
        agent_id: String,
        task_preview: String,
        /// 总步数（工具调用 + AI 回复），不受滑动窗口截断影响
        total_steps: usize,
        /// 滑动窗口，最多 4 条最近消息
        recent_messages: Vec<MessageViewModel>,
        /// 子 agent 执行中为 true
        is_running: bool,
        /// 默认展开，完成后用户可折叠
        collapsed: bool,
        /// SubAgentEnd 携带的结果摘要（工具返回值）
        final_result: Option<String>,
        /// SubAgent 执行是否以错误结束
        is_error: bool,
        /// 是否为后台 agent
        is_background: bool,
        /// Agent 实例的短显示标识符（6 位十六进制）
        bg_hash: Option<String>,
        /// 批次汇总信息：空 = 单 agent，非空 = 批次汇总模式
        batch_agents: Vec<AgentSummary>,
        /// Agent 实例的唯一标识符（用于聚焦模式过滤）
        instance_id: Option<String>,
        /// 预计算的语义 hash（构造/变更时更新，rebuild 直接读取避免重算）
        content_hash: u64,
    },
}

/// 语义级相等比较，用于判断 Done 时是否需要 RebuildAll。
///
/// 忽略 UI-only 字段（rendered、is_streaming、collapsed、color 等），
/// 只比较影响显示内容的字段。
impl PartialEq for MessageViewModel {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                MessageViewModel::UserBubble {
                    content: a,
                    system_reminder: a_sr,
                    ..
                },
                MessageViewModel::UserBubble {
                    content: b,
                    system_reminder: b_sr,
                    ..
                },
            ) => a == b && a_sr == b_sr,
            (
                MessageViewModel::AssistantBubble { blocks: a, .. },
                MessageViewModel::AssistantBubble { blocks: b, .. },
            ) => a == b,
            (
                MessageViewModel::ToolBlock {
                    tool_name: a_name,
                    tool_call_id: a_tc,
                    args_display: a_args,
                    content: a_content,
                    is_error: a_err,
                    diff_lines: a_diff,
                    ..
                },
                MessageViewModel::ToolBlock {
                    tool_name: b_name,
                    tool_call_id: b_tc,
                    args_display: b_args,
                    content: b_content,
                    is_error: b_err,
                    diff_lines: b_diff,
                    ..
                },
            ) => {
                a_name == b_name
                    && a_tc == b_tc
                    && a_args == b_args
                    && a_content == b_content
                    && a_err == b_err
                    && a_diff == b_diff
            }
            (
                MessageViewModel::SystemNote { content: a, .. },
                MessageViewModel::SystemNote { content: b, .. },
            ) => a == b,
            (
                MessageViewModel::CacheWarning { content: a, .. },
                MessageViewModel::CacheWarning { content: b, .. },
            ) => a == b,
            (
                MessageViewModel::ToolCallGroup {
                    category: a,
                    tools: a_tools,
                    ..
                },
                MessageViewModel::ToolCallGroup {
                    category: b,
                    tools: b_tools,
                    ..
                },
            ) => a == b && a_tools == b_tools,
            (
                MessageViewModel::SubAgentGroup {
                    agent_id: a_id,
                    task_preview: a_preview,
                    total_steps: a_steps,
                    recent_messages: a_msgs,
                    final_result: a_result,
                    is_error: a_err,
                    is_background: a_bg,
                    bg_hash: a_hash,
                    batch_agents: a_batch,
                    instance_id: a_instance_id,
                    ..
                },
                MessageViewModel::SubAgentGroup {
                    agent_id: b_id,
                    task_preview: b_preview,
                    total_steps: b_steps,
                    recent_messages: b_msgs,
                    final_result: b_result,
                    is_error: b_err,
                    is_background: b_bg,
                    bg_hash: b_hash,
                    batch_agents: b_batch,
                    instance_id: b_instance_id,
                    ..
                },
            ) => {
                a_id == b_id
                    && a_preview == b_preview
                    && a_steps == b_steps
                    && a_msgs == b_msgs
                    && a_result == b_result
                    && a_err == b_err
                    && a_bg == b_bg
                    && a_hash == b_hash
                    && a_batch == b_batch
                    && a_instance_id == b_instance_id
            }
            _ => false,
        }
    }
}

/// Hash 包含所有影响渲染输出的字段（内容 + UI 状态如 collapsed/is_streaming/is_running）。
/// `rendered` 和 `color` 不参与 hash（rendered 依赖宽度缓存，color 可从 tool_name 推导）。
impl Hash for MessageViewModel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            MessageViewModel::UserBubble {
                content,
                system_reminder,
                ..
            } => {
                0u8.hash(state);
                content.hash(state);
                system_reminder.hash(state);
            }
            MessageViewModel::AssistantBubble {
                blocks,
                is_streaming,
                collapsed,
                ..
            } => {
                1u8.hash(state);
                blocks.hash(state);
                is_streaming.hash(state);
                collapsed.hash(state);
            }
            MessageViewModel::ToolBlock {
                tool_name,
                tool_call_id,
                display_name,
                args_display,
                content,
                is_error,
                collapsed,
                diff_lines,
                ..
            } => {
                2u8.hash(state);
                tool_name.hash(state);
                tool_call_id.hash(state);
                display_name.hash(state);
                args_display.hash(state);
                content.hash(state);
                is_error.hash(state);
                collapsed.hash(state);
                diff_lines.hash(state);
            }
            MessageViewModel::SystemNote { content, .. } => {
                3u8.hash(state);
                content.hash(state);
            }
            MessageViewModel::CacheWarning { content, .. } => {
                4u8.hash(state);
                content.hash(state);
            }
            MessageViewModel::ToolCallGroup {
                category,
                tools,
                collapsed,
                ..
            } => {
                5u8.hash(state);
                category.hash(state);
                tools.hash(state);
                collapsed.hash(state);
            }
            MessageViewModel::SubAgentGroup {
                agent_id,
                task_preview,
                total_steps,
                recent_messages,
                is_running,
                collapsed,
                final_result,
                is_error,
                is_background,
                bg_hash,
                batch_agents,
                instance_id,
                ..
            } => {
                6u8.hash(state);
                agent_id.hash(state);
                task_preview.hash(state);
                total_steps.hash(state);
                recent_messages.hash(state);
                is_running.hash(state);
                collapsed.hash(state);
                final_result.hash(state);
                is_error.hash(state);
                is_background.hash(state);
                bg_hash.hash(state);
                batch_agents.hash(state);
                instance_id.hash(state);
            }
        }
    }
}

/// ContentBlock 的视图化表示
#[derive(Debug, Clone)]
pub enum ContentBlockView {
    /// 文本内容（含 markdown 解析缓存）
    Text {
        raw: String,
        rendered: Text<'static>,
        dirty: bool,
        /// 已渲染到 `raw` 的字节偏移（增量解析用）
        rendered_prefix_len: usize,
        /// `rendered` 中对应前缀的行数（避免重解析计数）
        rendered_prefix_lines: usize,
        /// 流式表格 holdback 扫描器
        holdback_scanner: crate::ui::markdown::TableHoldbackScanner,
    },
    /// 推理/思考过程（仅显示字数摘要，尾部预览可选）
    Reasoning {
        char_count: usize,
        /// 原始推理全文（仅用于提取尾部预览，不参与哈希/比较）
        text: String,
        /// 尾部行预览：符合条件时由后处理设置。
        /// 值为最后 3 行原始文本（不含 �� 前缀）。
        /// None = 不显示尾部预览
        tail_lines: Option<String>,
    },
    /// 工具使用请求（AI 发起的调用请求）
    ToolUse { name: String },
}

/// 只比较有意义的字段（忽略 `rendered`，因为 markdown 解析可能因宽度不同而异）
impl PartialEq for ContentBlockView {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                ContentBlockView::Text {
                    raw: a_raw,
                    dirty: a_dirty,
                    ..
                },
                ContentBlockView::Text {
                    raw: b_raw,
                    dirty: b_dirty,
                    ..
                },
            ) => a_raw == b_raw && a_dirty == b_dirty,
            (
                ContentBlockView::Reasoning { char_count: a, .. },
                ContentBlockView::Reasoning { char_count: b, .. },
            ) => a == b,
            (ContentBlockView::ToolUse { name: a }, ContentBlockView::ToolUse { name: b }) => {
                a == b
            }
            _ => false,
        }
    }
}

/// Hash 基于 PartialEq 的语义字段（忽略 `rendered` 缓存）。
/// 用于渲染线程的 hash diff：判断消息是否需要重新渲染。
impl Hash for ContentBlockView {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            ContentBlockView::Text { raw, dirty, .. } => {
                0u8.hash(state);
                raw.hash(state);
                dirty.hash(state);
            }
            ContentBlockView::Reasoning {
                char_count,
                tail_lines,
                ..
            } => {
                1u8.hash(state);
                char_count.hash(state);
                tail_lines.hash(state);
            }
            ContentBlockView::ToolUse { name } => {
                2u8.hash(state);
                name.hash(state);
            }
        }
    }
}

impl MessageViewModel {
    /// 追加流式文本 chunk
    pub fn append_chunk(&mut self, chunk: &str) {
        if let MessageViewModel::AssistantBubble {
            blocks, collapsed, ..
        } = self
        {
            // 如果有内容追加，自动展开
            if *collapsed && !chunk.is_empty() {
                *collapsed = false;
            }
            if let Some(ContentBlockView::Text { raw, dirty, .. }) = blocks.last_mut() {
                raw.push_str(chunk);
                *dirty = true;
                self.recompute_hash();
                return;
            }
            // 没有 Text block，创建新的
            let mut raw = String::new();
            raw.push_str(chunk);
            blocks.push(ContentBlockView::Text {
                raw,
                rendered: Text::raw(""),
                dirty: true,
                rendered_prefix_len: 0,
                rendered_prefix_lines: 0,
                holdback_scanner: Default::default(),
            });
            self.recompute_hash();
        }
    }

    /// 切换折叠状态（对 ToolBlock、AssistantBubble、SubAgentGroup、ToolCallGroup 生效）
    #[allow(dead_code)]
    pub fn toggle_collapse(&mut self) {
        match self {
            MessageViewModel::ToolBlock { collapsed, .. }
            | MessageViewModel::AssistantBubble { collapsed, .. }
            | MessageViewModel::SubAgentGroup { collapsed, .. }
            | MessageViewModel::ToolCallGroup { collapsed, .. } => {
                *collapsed = !*collapsed;
                self.recompute_hash();
            }
            _ => {}
        }
    }

    /// 判断是否为 AssistantBubble
    pub fn is_assistant(&self) -> bool {
        matches!(self, MessageViewModel::AssistantBubble { .. })
    }

    /// 判断是否为"仅含推理内容"的 AssistantBubble（渲染时不可见）
    /// 用于在工具分组合并时跳过中间的空 thinking bubble
    pub fn is_reasoning_only(&self) -> bool {
        match self {
            MessageViewModel::AssistantBubble { blocks, .. } => {
                blocks.is_empty()
                    || blocks.iter().all(|b| match b {
                        ContentBlockView::Reasoning { .. } => true,
                        ContentBlockView::Text { raw, .. } => raw.trim().is_empty(),
                        _ => false,
                    })
            }
            _ => false,
        }
    }

    /// 判断是否为 SubAgentGroup
    pub fn is_subagent_group(&self) -> bool {
        matches!(self, MessageViewModel::SubAgentGroup { .. })
    }

    /// 返回预计算的语义 hash
    pub fn content_hash(&self) -> u64 {
        match self {
            MessageViewModel::UserBubble { content_hash, .. } => *content_hash,
            MessageViewModel::AssistantBubble { content_hash, .. } => *content_hash,
            MessageViewModel::ToolBlock { content_hash, .. } => *content_hash,
            MessageViewModel::SystemNote { content_hash, .. } => *content_hash,
            MessageViewModel::CacheWarning { content_hash, .. } => *content_hash,
            MessageViewModel::ToolCallGroup { content_hash, .. } => *content_hash,
            MessageViewModel::SubAgentGroup { content_hash, .. } => *content_hash,
        }
    }

    /// 重新计算语义 hash（内容变更后调用）
    pub fn recompute_hash(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        let hash = hasher.finish();
        match self {
            MessageViewModel::UserBubble { content_hash, .. } => *content_hash = hash,
            MessageViewModel::AssistantBubble { content_hash, .. } => *content_hash = hash,
            MessageViewModel::ToolBlock { content_hash, .. } => *content_hash = hash,
            MessageViewModel::SystemNote { content_hash, .. } => *content_hash = hash,
            MessageViewModel::CacheWarning { content_hash, .. } => *content_hash = hash,
            MessageViewModel::ToolCallGroup { content_hash, .. } => *content_hash = hash,
            MessageViewModel::SubAgentGroup { content_hash, .. } => *content_hash = hash,
        }
    }
}

/// 从 SubAgent 返回结果中解析工具调用次数。
///
/// `format_subagent_result()` 输出格式：`[Sub-agent executed N tool calls: ...]`
/// 或中文版 `[子 agent 执行了 N 个工具调用: ...]`。
#[cfg(test)]
#[path = "message_view_test.rs"]
mod tests;
