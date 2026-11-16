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

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use peri_agent::interaction::UserInteractionBroker;
use peri_agent::middleware::prompt_sections::PromptSection;
use peri_agent::middleware::prompt_sections::PromptSectionZone;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::tools::BaseTool;

use crate::tools::AskUserTool;

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use peri_agent::interaction::InteractionContext;
    use peri_agent::interaction::InteractionResponse;
    use peri_agent::interaction::TrustedInteractionOwner;
    use peri_agent::interaction::UserInteractionBroker;
    use peri_agent::middleware::prompt_sections::PromptSectionZone;

    use super::*;

    /// 最小 broker stub（测试仅验证工具收集，不触发 request）。
    struct AutoAnswerBroker;

    #[async_trait]
    impl UserInteractionBroker for AutoAnswerBroker {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            _ctx: InteractionContext,
        ) -> InteractionResponse {
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

mod human_in_the_loop_middleware;
pub use human_in_the_loop_middleware::HumanInTheLoopMiddleware;
