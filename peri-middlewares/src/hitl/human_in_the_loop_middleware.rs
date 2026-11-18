use super::{
    fmt, Arc, AskUserTool, BaseTool, Debug, Formatter, Middleware, PromptSection,
    UserInteractionBroker,
};

// ─── HumanInTheLoopMiddleware（提问通道）──────────────────────────────────────

/// HumanInTheLoopMiddleware — 向用户提问的通道（持有 `AskUserQuestion` 工具）。
///
/// 唯一依赖是 [`UserInteractionBroker`]；无审批钩子。关闭本 middleware 后
/// `AskUserQuestion` 从 LLM 工具视图消失（与其余 middleware 工具一致的
/// 关闭语义）。
pub struct HumanInTheLoopMiddleware {
    pub(super) broker: Arc<dyn UserInteractionBroker>,
}

impl Debug for HumanInTheLoopMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HumanInTheLoopMiddleware")
            .finish_non_exhaustive()
    }
}

impl HumanInTheLoopMiddleware {
    /// 静态工具名（`assembly_test` 与 `MIDDLEWARE_TOOL_NAMES` 锁定用）。
    #[must_use]
    pub fn tool_names() -> Vec<&'static str> {
        vec!["AskUserQuestion"]
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// `12_ask_user` 段 = 提问纪律说明（`sections/12_ask_user.md`，
    /// include_str 零拷贝，文件在 `peri-middlewares/prompts/sections/`，与
    /// `10_hitl` 同模式）。契约 3（gate 原子迁移）：本段 gate = 本
    /// middleware 是否在链上（收集即装配）——关闭即段落消失。
    #[must_use]
    pub fn sections() -> Vec<PromptSection> {
        vec![PromptSection::declared(
            peri_acp_types::meta_harness::sections::ASK_USER,
            peri_agent::middleware::PromptSectionContent::Builtin(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/prompts/sections/12_ask_user.md"
            ))),
        )]
    }
}

impl HumanInTheLoopMiddleware {
    /// 创建提问通道 middleware（使用注入的 broker）。
    #[must_use]
    pub fn new(broker: Arc<dyn UserInteractionBroker>) -> Self {
        Self { broker }
    }
}

impl Middleware for HumanInTheLoopMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::AskUser.name()
    }

    /// 声明持有的系统提示词段落（12_ask_user，装配期收集，契约 2）。
    fn prompt_sections(&self) -> Vec<PromptSection> {
        Self::sections()
    }

    /// 提供 `AskUserQuestion` 工具（关闭即不装配 → 视图消失）。
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(AskUserTool::new(Arc::clone(&self.broker)))]
    }
}
