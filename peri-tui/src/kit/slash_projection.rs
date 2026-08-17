//! 投影 DTO：`SlashCommandEntry` 与 `ArgsSchema` 本地镜像（Phase 4 步骤 1）。
//!
//! TUI 手工 `serde_json` 解析 ACP `available_commands_update` 投影，不依赖
//! peri-acp-types schema 类型（沿用 acp_notifier 手工解析先例）。`ArgsSchema`
//! 镜像 Phase 1 `peri-acp-types::command::command_args` 的 serde 模型——序列化
//! 形态（externally tagged + 字段原样键名）由该文件锁定，wire 投影经 `_meta`
//! 通道携带本模型；Phase 3 的 peri-acp-types serde 模型落地后可直接替换，
//! 改动面仅限本模块与 acp_notifier 解析点。

use serde::{Deserialize, Serialize};

use crate::kit::slash_completion::SlashActionKind;

/// 投影条目（`available_commands_update.availableCommands[]` 元素）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandEntry {
    /// wire name 形态（Level1 裸名 / Level2 全名）：`compact` / `demo:hello`
    /// （Level1 core/ui 域投影即裸名，不携带域前缀；域归属经 `kind` / `level`
    /// 区分）。
    pub fullname: String,
    /// 投影 kind（复用渲染枚举——数据/渲染同源，消除两套枚举映射）。
    pub kind: SlashActionKind,
    pub description: String,
    pub aliases: Vec<String>,
    /// 自由文本分类（wire 缺省 = None）。
    pub category: Option<String>,
    /// 参数 schema（wire 缺省 = None）。
    pub args: Option<ArgsSchema>,
    /// 展示等级：1 = 裸名（core/ui 域）；2 = 全名（mcp/plugin/user 域）。
    pub level: u8,
}

impl Default for SlashCommandEntry {
    /// 缺省条目 = 无 `_meta` 元数据投影的回退形态（kind=Command / level=1，
    /// 与 acp_notifier 缺省回退语义一致）。`SlashActionKind` 无 Default，
    /// 故手工实现而非 derive。
    fn default() -> Self {
        Self {
            fullname: String::new(),
            kind: SlashActionKind::Command,
            description: String::new(),
            aliases: Vec::new(),
            category: None,
            args: None,
            level: 1,
        }
    }
}

/// 命令参数 schema（设计 docs/design/command-system.md §73「Execution 层」）。
///
/// 三个维度全部可选（缺省 = 空），`#[serde(default)]` 保证 wire 兼容：
/// 旧投影不含 args 字段时反序列化即得全默认。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgsSchema {
    /// 位置参数（按声明顺序匹配）。
    #[serde(default)]
    pub positionals: Vec<ArgSpec>,
    /// 命名参数（按 `name` 字段匹配）。
    #[serde(default)]
    pub named: Vec<ArgSpec>,
    /// 布尔开关参数（presence-only）。
    #[serde(default)]
    pub flags: Vec<FlagSpec>,
}

/// 位置参数 / 命名参数共用形态（与 Phase 1 `peri-acp-types::command_args::ArgSpec`
/// 同构：字段顺序 name/required/kind/description，serde 无关，纯一致性）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// 参数名（positional 为形参名，named 为 `--name` 的长形态）。
    pub name: String,
    /// 是否必填（缺省 = 可选）。
    #[serde(default)]
    pub required: bool,
    /// 参数值类型。
    pub kind: ArgKind,
    /// 人类可读描述（补全列表 / 校验器错误信息复用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 布尔开关参数（presence-only）；**与 wire 的 `periArgs.flags` 元素对齐**
/// （P1-5）：镜像 Phase 1 `FlagSpec` 的 serde 形态——object 数组而非字符串
/// 数组，否则 `serde_json::from_value::<ArgsSchema>` 对 `periArgs` 反序列化
/// 必然失败。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagSpec {
    /// 长形态（如 `--force`）。
    pub name: String,
    /// 短形态（如 `-f`，含连字符的展示形态；wire 原样携带）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    /// 人类可读描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 参数值类型（设计 §73：String | Int | Choice | Path；serde 形态镜像
/// Phase 1 `ArgKind`——externally tagged、变体名原样，不设 `rename_all`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgKind {
    /// 自由文本。
    String,
    /// 整数。
    Int,
    /// 枚举候选（空候选列表 = 运行时由 handler 补充候选）。
    Choice(Vec<String>),
    /// 文件路径（第一版校验存在性，补全留待 TUI 能力）。
    Path,
}

/// wire kind（Phase 1 `CommandEntryKind` snake_case 形态）→ 渲染枚举。
///
/// 未知 / 缺失 → `None`，调用方回退 `SlashActionKind::Command`（R1）。
pub fn parse_projection_kind(s: &str) -> Option<SlashActionKind> {
    match s {
        "command" => Some(SlashActionKind::Command),
        "skill" => Some(SlashActionKind::Skill),
        "mcp_skill" => Some(SlashActionKind::McpSkill),
        "panel" => Some(SlashActionKind::Panel),
        _ => None,
    }
}

/// 分级展示名（「权威词法」：display 即 lexical——用户提交的文本与显示一致）。
///
/// level 1 → 最右冒号后段裸名（`core:compact` → `compact`）；level 2 → 全名
/// 原样（`demo:hello`）。无冒号 / 非 1 级一律返回全名原样，保证提交文本
/// 与显示一致。
///
/// 协议层投影已按本规则输出（Level1 裸名 / Level2 全名，TUI/stdio 同一条
/// 实现），本函数对裸名输入幂等原样返回——保留以兼容旧形态 fullname
/// （如历史缓存或直接构造的 SlashCommandEntry）。
pub fn display_name(fullname: &str, level: u8) -> String {
    if level == 1 {
        fullname
            .rsplit_once(':')
            .map(|(_, bare)| bare)
            .unwrap_or(fullname)
            .to_string()
    } else {
        fullname.to_string()
    }
}

#[cfg(test)]
#[path = "slash_projection_test.rs"]
mod tests;
