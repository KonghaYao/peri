//! 语义令牌：表达颜色用途。
//!
//! 定义文字、边框、状态、表面、Diff 五种语义令牌。
//! 每类令牌在主题定义中映射调色板的具体色值。

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// 语义令牌集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTokens {
    pub accent: Color,
    /// 消息流角色强调色（§4 表：user/assistant/reasoning/tool）。
    /// `primary` 与旧 `accent` 同值同源，旧字段保留。
    #[serde(default)]
    pub accents: AccentTokens,
    pub text: TextTokens,
    pub border: BorderTokens,
    pub status: StatusTokens,
    pub surface: SurfaceTokens,
    pub diff: DiffTokens,
    /// shell 命令 / 文件路径语义色（§4 表）。
    #[serde(default)]
    pub syntax: SyntaxTokens,
    pub loading: Color,
    pub thinking: Color,
    pub model_info: Color,
    /// 模型名内嵌 effort 后缀色（如 "gpt-5.6-luna high" 的 "high"）
    pub model_accent: Color,
    /// effort 档位值色（low/medium/high/xhigh/max）
    pub effort: Color,
    /// 上下文窗口标识色（200k / 1m）
    pub token_context: Color,
    pub bash_border: Color,
    pub selected_fg: Color,
}

/// 消息流角色强调色（§4 表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccentTokens {
    /// 主强调色——与 `SemanticTokens::accent` 同值同源。
    pub primary: Color,
    /// 用户 prompt。
    pub user: Color,
    /// assistant 回答。
    pub assistant: Color,
    /// reasoning 过程。
    pub reasoning: Color,
    /// 已完成的 tool。
    pub tool: Color,
}

/// shell 命令 / 文件路径语义色（§4 表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SyntaxTokens {
    /// shell command。
    pub command: Color,
    /// 文件路径。
    pub path: Color,
}

/// 文字四级亮度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextTokens {
    pub primary: Color,
    /// 次级正文（§4 表）。
    #[serde(default)]
    pub secondary: Color,
    pub muted: Color,
    pub dim: Color,
}

/// 边框三级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorderTokens {
    pub default: Color,
    pub active: Color,
    pub dim: Color,
}

/// 状态四色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusTokens {
    pub running: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

/// 表面七层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceTokens {
    pub default: Color,
    /// 抬升表面（composer、expanded tool body，§4 表）。
    #[serde(default)]
    pub raised: Color,
    /// 下沉表面（code、terminal output，§4 表）。
    #[serde(default)]
    pub sunken: Color,
    pub user: Color,
    pub popup: Color,
    pub selection: Color,
    pub cursor: Color,
}

/// Diff 语义 7 色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffTokens {
    pub add: Color,
    pub remove: Color,
    pub hunk: Color,
    pub add_bg: Color,
    pub remove_bg: Color,
    pub add_word_bg: Color,
    pub remove_word_bg: Color,
}
