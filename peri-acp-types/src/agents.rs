//! agent 定义契约（agent.md 覆盖项 + 能力标签）。
//!
//! 自 `peri-middlewares`（`agent_define` / `scan_agents_detailed`）迁入
//! （3.0 批 2 波 1：协议类型归契约层；middlewares 保留 re-export 保兼容）。

/// agent.md 中可覆盖 system prompt 的部分
///
/// 所有字段均为 `Option`，`None` 表示使用默认值。
#[derive(Debug, Clone, Default)]
pub struct AgentOverrides {
    /// 角色定位（替换 `{{persona}}`）
    pub persona: Option<String>,
    /// 输出风格（替换 `{{tone_and_style}}`）
    pub tone: Option<String>,
    /// 主动性（替换 `{{proactiveness}}`）
    pub proactiveness: Option<String>,
    /// agent.md frontmatter 中 prompt_mode 的值："extend"|"full"，默认 extend。
    /// `full` 只替换 PersonaDomain 层（persona/domain instructions）；
    /// 安全与授权、工程行为、能力契约与运行时边界层始终保留，不会被移除。
    pub mode: Option<String>,
}

impl AgentOverrides {
    pub fn is_empty(&self) -> bool {
        self.persona.is_none() && self.tone.is_none() && self.proactiveness.is_none()
    }
}

/// agent 可调度的模型档位集合（与 `peri-acp` `Profiles::ALL` 内容一致；
/// `inherit` 是工具参数语义而非档位，不在此集合内）。
///
/// 单一事实源：Agent 工具 `model` 参数白名单与 subagent catalog 展示均引用
/// 此常量，避免跨 crate 硬编码漂移。顺序（弱 → 强）用于展示，无调度语义。
pub const MODEL_TIERS: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

/// 模型档位 typed 值（[`MODEL_TIERS`] 的枚举形态）。
///
/// 构造只能经 [`ModelTier::parse`] 或 [`AgentModelSelection::parse`]，因此
/// 「已验证档位」在类型上可传递：目录渲染与执行装配不再消费原始 YAML 文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTier {
    Haiku,
    Sonnet,
    Opus,
    Fable,
}

impl ModelTier {
    /// 全部档位（弱 → 强；与 [`MODEL_TIERS`] 同序）。
    pub const ALL: [ModelTier; 4] = [
        ModelTier::Haiku,
        ModelTier::Sonnet,
        ModelTier::Opus,
        ModelTier::Fable,
    ];

    pub fn as_str(self) -> &'static str {
        MODEL_TIERS[self as usize]
    }

    /// 大小写不敏感解析（首尾空白忽略）；未知值返回 `None`，不猜测、不截断。
    pub fn parse(raw: &str) -> Option<ModelTier> {
        let normalized = raw.trim();
        ModelTier::ALL
            .into_iter()
            .find(|tier| tier.as_str() == normalized.to_ascii_lowercase())
    }
}

/// frontmatter / 工具参数 `model` 的 typed 解析结果。
///
/// 区分三种语义：未指定（字段省略或空白）、显式 `inherit`、合法档位；
/// **非法值不构造本枚举**，由 [`AgentModelSelection::parse`] 返回 typed 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentModelSelection {
    /// 未指定：沿用既有语义（执行时按继承父模型处理，展示为 `inherit`）。
    #[default]
    Unspecified,
    /// 显式 `inherit`。
    Inherit,
    /// 合法档位。
    Tier(ModelTier),
}

/// 非法模型档位错误。
///
/// 只携带错误类别：不回显可疑原始值（注入形状/秘密不得经错误文本或日志外泄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unsupported model tier (expected one of: inherit, haiku, sonnet, opus, fable)")]
pub struct InvalidModelTier;

impl AgentModelSelection {
    /// typed 解析：`None`/空白/`inherit`/合法档位接受，其余返回
    /// [`InvalidModelTier`]（不静默降级为 inherit）。
    pub fn parse(raw: Option<&str>) -> Result<AgentModelSelection, InvalidModelTier> {
        let Some(raw) = raw else {
            return Ok(AgentModelSelection::Unspecified);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(AgentModelSelection::Unspecified);
        }
        if trimmed.eq_ignore_ascii_case("inherit") {
            return Ok(AgentModelSelection::Inherit);
        }
        ModelTier::parse(trimmed)
            .map(AgentModelSelection::Tier)
            .ok_or(InvalidModelTier)
    }

    /// 目录/提示渲染标签：只能是 `'static` 档位名或 `inherit`。
    pub fn catalog_label(self) -> &'static str {
        match self {
            AgentModelSelection::Tier(tier) => tier.as_str(),
            AgentModelSelection::Unspecified | AgentModelSelection::Inherit => "inherit",
        }
    }

    /// 执行面档位别名：`None` = 继承父模型（未指定或显式 inherit）。
    pub fn tier_alias(self) -> Option<&'static str> {
        match self {
            AgentModelSelection::Tier(tier) => Some(tier.as_str()),
            AgentModelSelection::Unspecified | AgentModelSelection::Inherit => None,
        }
    }

    /// 大小写归一后的 frontmatter 写回形态（`inherit` 保留显式语义）。
    pub fn normalized_value(self) -> Option<&'static str> {
        match self {
            AgentModelSelection::Unspecified => None,
            AgentModelSelection::Inherit => Some("inherit"),
            AgentModelSelection::Tier(tier) => Some(tier.as_str()),
        }
    }
}

/// 主提示词 `{{available_agents}}` 候选目录的一条（W5：由资源面投影派生）。
///
/// 迁移前该目录来自宿主本地扫盘（`SkillsPort::agents` → `scan_agents_detailed`）；
/// W5 起唯一来源是会话级 MCP Agent registry 对 builtin `workspace` 实例
/// `resources/list` 的投影（本地三来源 + E13 优先级去重 + builtin 开关过滤）。
/// 结构只承载渲染所需字段：`id`、`model_tier`、`can_mutate`（描述不注入，
/// 见 `format_available_agents` 的既有口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCatalogEntry {
    /// `subagent_type` 参数值（本地来源恒为裸 agent 标识）。
    pub id: String,
    /// 模型档位（已验证 typed 值；渲染只取 [`AgentModelSelection::catalog_label`]）。
    pub model_tier: AgentModelSelection,
    /// 保守写能力标签（调度提示，不是授权）。
    pub can_mutate: bool,
}

/// agent 能力标签（subagent catalog 检索依据；由 agent.md 推断）。
///
/// - 能否并行执行（readonly agent 可安全并发）
/// - 质量/成本/延迟预期（模型级别）
///
/// `can_mutate` 是**保守调度提示**，不是代码级锁或安全边界：
/// 实际能力由 `filter_tools` 在工具注册层真裁剪，标签仅间接影响主模型
/// 的并行决策（见审计 prompt-sections-audit.md P1-8 修正后判定）。
#[derive(Debug, Clone)]
pub struct AgentCapability {
    /// 模型级别：typed 已验证值（`haiku` / `sonnet` / `opus` / `fable` / `inherit`）
    pub model_tier: AgentModelSelection,
    /// 该 agent 是否会修改项目代码（保守推断，D5）。
    /// 只有能根据最终注册工具集合证明无项目写能力时才为 false：
    /// - omitted tools（继承父工具）含 Bash / folder_operations 等 → true，
    ///   除非显式 disallow 全部核心写能力工具；
    /// - 显式 `tools: []` → false（零工具）；
    /// - 白名单含任一写能力工具（Bash / Write / Edit / folder_operations /
    ///   cron_register / mcp__*）→ true。
    ///
    /// `allowedWriteDirs` 声明的 WriteSandbox 不计入 can_mutate，
    /// 因为沙箱目录不在项目代码范围内，agent 仍可并行调度。
    pub can_mutate: bool,
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;
