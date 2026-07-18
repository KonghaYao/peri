//! Claude Code Agent 文件解析器
//!
//! 解析 Claude Code 格式的 agent 定义文件（Markdown with YAML frontmatter）
//!
//! 文件格式示例：
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Reviews code for quality and best practices
//! tools: Read, Glob, Grep
//! model: sonnet
//! ---
//!
//! You are a code reviewer...
//! ```

use serde::Deserialize;

/// Claude Code Agent YAML frontmatter 定义
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAgentFrontmatter {
    /// 使用小写字母和连字符的唯一标识符
    pub name: String,
    /// Claude 何时应委托给此 subagent
    pub description: String,
    /// subagent 可以使用的工具列表（逗号分隔字符串或数组）
    #[serde(default)]
    pub tools: ToolsValue,
    /// 要拒绝的工具列表
    #[serde(default)]
    pub disallowed_tools: ToolsValue,
    /// 使用的模型：sonnet、opus、haiku 或 inherit
    #[serde(default)]
    pub model: Option<String>,
    /// 输出风格覆盖（替换默认的 Tone and style 章节）
    #[serde(default)]
    pub tone: Option<String>,
    /// 主动性覆盖（替换默认的 Proactiveness 章节）
    #[serde(default)]
    pub proactiveness: Option<String>,
    /// 权限模式：default、acceptEdits、dontAsk、bypassPermissions 或 plan
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// subagent 停止前的最大代理轮数
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// 在启动时加载的 skills 列表
    #[serde(default)]
    pub skills: Vec<String>,
    /// MCP servers 配置
    #[serde(default)]
    pub mcp_servers: Vec<serde_yaml::Value>,
    /// Hooks 配置
    #[serde(default)]
    pub hooks: serde_yaml::Value,
    /// 持久内存范围：user、project 或 local
    #[serde(default)]
    pub memory: Option<String>,
    /// 是否始终在后台运行
    #[serde(default)]
    pub background: bool,
    /// Git worktree 隔离模式
    #[serde(default)]
    pub isolation: Option<String>,
    /// 沙箱写目录白名单——声明后 subagent 可获得 WriteSandbox 工具，
    /// 只能写入这些相对目录（基于 cwd），不能碰项目代码。
    /// 不影响 can_mutate 推断（agent 仍视为 readonly）。
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
}

/// 工具列表，可以是逗号分隔字符串或数组
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ToolsValue {
    #[default]
    Empty,
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for ToolsValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::String(s) => {
                // 解析逗号分隔的字符串
                let tools: Vec<String> = s
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                Ok(ToolsValue::List(tools))
            }
            serde_yaml::Value::Sequence(arr) => {
                let tools: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|t| !t.is_empty())
                    .collect();
                Ok(ToolsValue::List(tools))
            }
            _ => Ok(ToolsValue::Empty),
        }
    }
}

impl ToolsValue {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            ToolsValue::Empty => Vec::new(),
            ToolsValue::List(v) => v.clone(),
        }
    }
}

impl ClaudeAgent {
    /// 获取工具列表
    pub fn tools(&self) -> Vec<String> {
        self.frontmatter.tools.to_vec()
    }

    /// 获取被拒绝的工具列表
    pub fn disallowed_tools(&self) -> Vec<String> {
        self.frontmatter.disallowed_tools.to_vec()
    }
}

/// Claude Code Agent 定义
#[derive(Debug, Clone)]
pub struct ClaudeAgent {
    /// Frontmatter 配置
    pub frontmatter: ClaudeAgentFrontmatter,
    /// Markdown 正文（系统提示）
    pub system_prompt: String,
}

/// 将 agent_id（kebab-case 或 snake_case）格式化为友好显示名称
///
/// 例：`"code-reviewer"` → `"Code Reviewer"`，`"security_auditor"` → `"Security Auditor"`
pub fn format_agent_id(id: &str) -> String {
    id.split(['-', '_'])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 解析 Claude Code agent 文件内容
///
/// 返回 frontmatter 和 markdown 正文
pub fn parse_agent_file(content: &str) -> Option<ClaudeAgent> {
    parse_agent_file_inner(content)
        .map_err(|e| {
            tracing::warn!("agent 文件解析失败: {e}");
            e
        })
        .ok()
}

/// 内部实现，返回具体错误信息
fn parse_agent_file_inner(content: &str) -> Result<ClaudeAgent, String> {
    // Normalize line endings to LF to avoid CRLF-related byte offset issues on Windows.
    let content = content.replace("\r\n", "\n");
    let content = content.trim();

    if !content.starts_with("---") {
        return Err("文件不以 '---' 开头，缺少 YAML frontmatter".to_string());
    }

    // 按行查找闭合 "---"，避免匹配 YAML 值中的行内 ---
    let after_open = &content[3..];
    let close_pos = after_open
        .lines()
        .enumerate()
        .skip_while(|(_, line)| line.trim().is_empty())
        .find(|(_, line)| line.trim() == "---")
        .map(|(i, _)| {
            // 计算到该行末尾的字节偏移
            after_open
                .lines()
                .take(i + 1)
                .map(|l| l.len() + 1) // +1 for the '\n' stripped by .lines()
                .sum::<usize>()
        })
        .ok_or_else(|| "未找到闭合的 '---' 分隔符".to_string())?;

    let yaml_content = &after_open[..close_pos.saturating_sub(4)]; // 减去 "---\n"
    let markdown_content = after_open[close_pos..].trim();

    let frontmatter: ClaudeAgentFrontmatter = serde_yaml::from_str(yaml_content)
        .map_err(|e| format!("YAML frontmatter 解析失败: {e}"))?;

    Ok(ClaudeAgent {
        frontmatter,
        system_prompt: markdown_content.to_string(),
    })
}

#[cfg(test)]
#[path = "claude_agent_parser_test.rs"]
mod tests;
