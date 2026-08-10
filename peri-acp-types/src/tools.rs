//! 工具契约类型（自 peri-agent 迁入，`peri-agent::tools` 保留 re-export）。

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::messages::BaseMessage;

/// Todo 条目状态（L5：自 `peri-middlewares/src/tools/todo.rs` 迁入，
/// middlewares 保留 re-export；与 `crate::event::TodoStatus`（事件 DTO）同构
/// 但独立定义，避免改动事件序列化语义）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Todo 列表条目（L5：自 `peri-middlewares/src/tools/todo.rs` 迁入契约层，
/// TodoWrite 工具 / 装配上下文 todo 通道共用；middlewares 保留 re-export）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

/// 工具定义（JSON Schema 格式参数描述）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
}

/// 工具描述契约（design v2 §2.5.1，v1.4 新增）
///
/// 提示词层声明与 UI 展示使用的结构化描述。线上 LLM 投影仍为
/// [`ToolDefinition`]（name/description/parameters）——`title`/`namespace`
/// 仅存在于进程内契约与提示词层，不下发 API（OpenAI/Anthropic function
/// calling 无对应字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDescription {
    /// 模型调用名（必填，与 `BaseTool::name()` 一致）
    pub name: String,
    /// 模型向完整描述（必填，进入 API tools 列表）
    pub description: String,
    /// 短显示名（提示词层声明与 UI 展示引用；缺省由 [`derive_title_from_name`] 推导）
    pub title: Option<String>,
    /// 分组（提示词层按组组织声明段；缺省不分组）
    pub namespace: Option<String>,
}

/// 从工具名推导短显示名：CamelCase / snake_case 拆词、词首大写。
///
/// - `AskUserQuestion` → `Ask User Question`
/// - `folder_operations` → `Folder Operations`
/// - `Read` → `Read`
///
/// 仅处理 ASCII 字母数字与 `_`；连续大写/数字等超范围输入按最小规则尽力拆分
/// （design v2 §2.5.1「缺省时由 name 推导」）。
pub fn derive_title_from_name(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    // 前一个字符是否小写——camelCase 边界（小写 → 大写）据此切词
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
        } else if c.is_ascii_uppercase() && prev_lower {
            words.push(std::mem::take(&mut current));
            current.push(c);
            prev_lower = false;
        } else {
            current.push(c);
            prev_lower = c.is_ascii_lowercase();
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut capitalized = String::with_capacity(word.len());
                    capitalized.push(first.to_ascii_uppercase());
                    capitalized.push_str(chars.as_str());
                    capitalized
                }
                None => word,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 工具上下文保留策略（用于 Compact 决策；自 peri-agent 迁入，
/// `peri-agent::tools::ContextRetention` 保留 re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRetention {
    /// 必须完整保留（用户回答、目标、任务状态工具）
    Preserve,
    /// 后续控制流依赖的状态（后续可能降级但不是现在）
    StateBearing,
    /// 副作用已完成的收据（只需保留摘要/状态）
    SideEffectReceipt,
    /// 可从磁盘/网络重新获取
    Recomputable,
}

/// 工具只读上下文（借用 state，零 clone）
///
/// 通过 `BaseTool::invoke` 的第二个参数传入。工具可读取 messages 和 cwd，
/// 但不能修改 state（避免绕过 dispatch_tools 统一写入语义）。
pub struct ToolContext<'a> {
    /// 当前对话历史（只读引用，借用 state.messages）
    pub messages: &'a [BaseMessage],
    /// 当前工作目录
    pub cwd: &'a str,
}

impl<'a> ToolContext<'a> {
    pub fn new(messages: &'a [BaseMessage], cwd: &'a str) -> Self {
        Self { messages, cwd }
    }
}

/// BaseTool trait - 对齐 LangChain Python BaseTool
///
/// 所有工具必须实现此 trait，不再依赖 langchain-rust::tools::Tool。
#[async_trait]
pub trait BaseTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// 返回完整工具定义（默认实现，组合 name/description/parameters）
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// 执行工具，输入为 JSON Value
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    /// 工具调用的外层超时时间。None 表示不设外层超时（工具自行管理超时）。
    /// 默认 120s，适用于 Read/Edit/Glob 等快速操作。Agent/Bash 等工具应返回
    /// None，因为它们内部已有超时机制或需要长时间运行。
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }

    /// 工具声明的别名列表。当 LLM 输出的工具名匹配这些别名（大小写无关）时，
    /// 由 resolve_tool() 解析到本工具。典型用例：BashTool → aliases=["Shell"]。
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// 工具输出的默认截断长度（字符数）。None 表示不截断。
    fn output_char_limit(&self) -> Option<usize> {
        None
    }

    /// 工具输出是否偏向落盘而非内联返回。
    fn prefers_persist(&self) -> bool {
        false
    }

    /// 工具在上下文压缩中的保留策略。
    ///
    /// 默认返回 `ContextRetention::Preserve`——未显式标注的工具绝对不会被压缩。
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::Preserve
    }

    /// 是否直接出现在 LLM 的 tools 参数中（无需经过 SearchExtraTools 发现）。
    /// 默认 `false`（安全默认值：新工具默认为 deferred）。
    fn is_direct(&self) -> bool {
        false
    }

    /// 短显示名（≤ 6 词，名词短语）。缺省时由 [`derive_title_from_name`] 推导。
    fn title(&self) -> Option<&str> {
        None
    }

    /// 工具分组（如 `filesystem`、`web`、`meta`）。缺省不分组。
    fn namespace(&self) -> Option<&str> {
        None
    }

    /// 结构化工具描述（design v2 §2.5.1）：组装 name/description/title/namespace，
    /// title 缺省时由 name 推导。
    fn tool_description(&self) -> ToolDescription {
        ToolDescription {
            name: self.name().to_string(),
            description: self.description().to_string(),
            title: Some(
                self.title()
                    .map(str::to_owned)
                    .unwrap_or_else(|| derive_title_from_name(self.name())),
            ),
            namespace: self.namespace().map(str::to_owned),
        }
    }

    /// 提示词层声明模板；返回 `None` 表示不出现在提示词声明段（默认）。
    fn prompt_declaration(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
