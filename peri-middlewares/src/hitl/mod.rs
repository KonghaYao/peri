//! HumanInTheLoopMiddleware — 提问通道（"人在环"的主动提问路径）。
//!
//! 2026-08-15 职责拆分（`spec/issues/2026-08-15-permission-hitl-split.md`）：
//! 原审批职责（`before_tool` 钩子 + `PermissionMode` + `AutoClassifier` +
//! `10_hitl` 段落）移交 `PermissionMiddleware`（`crate::permission`）；本
//! middleware 接管旧名 `HumanInTheLoopMiddleware`，专司人机交互的**提问**
//! 路径：持有 `AskUserQuestion` 工具（`collect_tools` 提供）与
//! `12_ask_user` 段落。
//!
//! meta harness 关闭语义（设计 §2.5）：disabled 时不装配 → 链无本
//! middleware → `collect_tools` 无 `AskUserQuestion` → 每 turn 本地视图
//! 不含 → LLM 视图不可见（此前 AskUserQuestion 游离于 middleware 体系外、
//! 无条件注册宿主级 `shared_tools`，"关闭不掉"，本拆分修复）。
//!
//! 与 `PermissionMiddleware` 的关系：审批（`ApprovalItem`）与提问
//! （`QuestionItem`）共用 `UserInteractionBroker` 契约但方向相反——审批
//! 拒绝后 agent 靠本通道追问用户，两者可独立开关。

use std::sync::Arc;

use peri_agent::{
    interaction::UserInteractionBroker,
    middleware::{
        prompt_sections::{PromptSection, PromptSectionZone},
        r#trait::Middleware,
    },
    tools::BaseTool,
};

use crate::tools::AskUserTool;

// ─── HumanInTheLoopMiddleware（提问通道）──────────────────────────────────────

/// HumanInTheLoopMiddleware — 向用户提问的通道（持有 `AskUserQuestion` 工具）。
///
/// 唯一依赖是 [`UserInteractionBroker`]；无审批钩子。关闭本 middleware 后
/// `AskUserQuestion` 从 LLM 工具视图消失（与其余 middleware 工具一致的
/// 关闭语义）。
pub struct HumanInTheLoopMiddleware {
    broker: Arc<dyn UserInteractionBroker>,
}

impl HumanInTheLoopMiddleware {
    /// 静态工具名（`assembly_test` 与 `MIDDLEWARE_TOOL_NAMES` 锁定用）。
    pub fn tool_names() -> Vec<&'static str> {
        vec!["AskUserQuestion"]
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// `12_ask_user` 段 = 提问纪律说明（`sections/12_ask_user.md`，
    /// include_str 零拷贝，文件在 `peri-acp/prompts/sections/`，与
    /// `10_hitl` 同模式）。契约 3（gate 原子迁移）：本段 gate = 本
    /// middleware 是否在链上（收集即装配）——关闭即段落消失。
    pub fn sections() -> Vec<PromptSection> {
        vec![PromptSection::builtin(
            "12_ask_user",
            PromptSectionZone::Uncached,
            5, // 2026-08-15 拆分后编号事实：gated 12=5（11_subagent=4 之后，13_skills 顺延为 6）
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../peri-acp/prompts/sections/12_ask_user.md"
            )),
        )]
    }
}

impl HumanInTheLoopMiddleware {
    /// 创建提问通道 middleware（使用注入的 broker）。
    ///
    /// 注意：**使用原始 broker**，不使用 MultiplexBroker——ChannelBroker 对
    /// Questions 立即返回空答案、Multiplex 竞速时 Channel 先返回，会绕过
    /// TUI 弹窗（装配期注释的既有约束，`assembly.rs`）。
    pub fn new(broker: Arc<dyn UserInteractionBroker>) -> Self {
        Self { broker }
    }
}

impl Middleware for HumanInTheLoopMiddleware {
    fn name(&self) -> &str {
        "HumanInTheLoopMiddleware"
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

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use peri_agent::{
        interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
        middleware::prompt_sections::PromptSectionZone,
    };

    use super::*;

    /// 最小 broker stub（测试仅验证工具收集，不触发 request）。
    struct AutoAnswerBroker;

    #[async_trait]
    impl UserInteractionBroker for AutoAnswerBroker {
        async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
            InteractionResponse::Decisions(vec![])
        }
    }

    fn mock_broker() -> Arc<dyn UserInteractionBroker> {
        Arc::new(AutoAnswerBroker)
    }

    #[test]
    fn name_and_tool_names() {
        let mw = HumanInTheLoopMiddleware::new(mock_broker());
        assert_eq!(mw.name(), "HumanInTheLoopMiddleware");
        assert_eq!(
            HumanInTheLoopMiddleware::tool_names(),
            vec!["AskUserQuestion"]
        );
    }

    #[test]
    fn collect_tools_provides_ask_user_question() {
        let mw = HumanInTheLoopMiddleware::new(mock_broker());
        let tools = mw.collect_tools("/tmp");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "AskUserQuestion");
        assert!(
            tools[0].is_direct(),
            "AskUserQuestion 必须 is_direct（Core 层）"
        );
    }

    #[test]
    fn ask_user_section_declaration_shape() {
        let sections = HumanInTheLoopMiddleware::sections();
        assert_eq!(sections.len(), 1, "12_ask_user 段应唯一");
        let section = &sections[0];
        assert_eq!(section.id, "12_ask_user");
        assert_eq!(section.zone, PromptSectionZone::Uncached);
        assert_eq!(section.order, 5);
    }
}
