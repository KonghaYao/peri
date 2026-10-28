//! Smart selection uses the same Micro policy and durable commit boundary.
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ContextPressure;
use crate::error::AgentResult;
use crate::session::transcript::MessageTranscript;

/// Deprecated convenience entry：复用统一收益准入，禁止「先 apply，再判断收益」。
///
/// 没有当前运行期消费证据时计划为空，保持历史；没有上下文窗口（C == 0）时
/// 统一决策会延后，不执行比例型自动 Micro。
pub async fn smart_compact(
    transcript: &mut MessageTranscript,
    config: &CompactConfig,
    pressure: &ContextPressure,
) -> AgentResult<(usize, u64)> {
    tracing::warn!("Smart Compact is deprecated; using the unified Micro admission decision");
    // 兼容入口没有可失败调用方：规划错误按空计划处理（保持历史），不伪装为收益不足。
    let plan = super::planner::plan_micro(transcript, config, true);
    let decision = super::decision::decide_auto_compact(
        pressure,
        config,
        plan.estimated_tokens_saved,
        plan.has_changes(),
    );
    if decision != super::decision::GainDecision::Micro {
        return Ok((0, 0));
    }
    let affected = super::micro::apply_micro_plan(transcript, &plan).await?;
    Ok((affected, plan.estimated_tokens_saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::BaseMessage;

    fn pressure() -> ContextPressure {
        ContextPressure {
            estimated_tokens: 0,
            context_window: 0,
            output_reserve: 0,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        }
    }

    #[tokio::test]
    async fn test_smart_without_exposure_cannot_infer_consumption_from_age() {
        let mut transcript = MessageTranscript::new();
        for i in 0..100 {
            transcript.append(BaseMessage::human(format!("task {i}")));
            transcript.append(BaseMessage::tool_result(
                format!("call-{i}"),
                "old".repeat(1000),
            ));
        }
        let revision = transcript.view_revision();
        assert_eq!(
            smart_compact(&mut transcript, &CompactConfig::default(), &pressure())
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(transcript.view_revision(), revision);
    }

    #[tokio::test]
    async fn test_smart_without_context_budget_defers_instead_of_zero_threshold_micro() {
        // 没有上下文窗口就没有比例门槛的权威事实：即使配置阈值为 0，
        // 也不能把 M == 0 当成「任意收益都通过」。
        let mut transcript = MessageTranscript::new();
        for i in 0..8 {
            transcript.append(BaseMessage::human(format!("task {i}")));
            transcript.append(BaseMessage::tool_result(
                format!("call-{i}"),
                "x".repeat(16_000),
            ));
        }
        let revision = transcript.view_revision();
        let config = CompactConfig {
            micro_compact_threshold: 0.0,
            ..Default::default()
        };
        assert_eq!(
            smart_compact(&mut transcript, &config, &pressure())
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(transcript.view_revision(), revision);
    }
}
