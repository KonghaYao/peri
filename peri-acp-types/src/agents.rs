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
    /// 模型级别：`haiku` / `sonnet` / `opus` / `fable` / `inherit`
    pub model_tier: String,
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
