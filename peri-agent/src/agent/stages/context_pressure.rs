//! 以实际可见请求视图补齐 provider usage 之间的增长及冷启动预算。

use crate::agent::stages::StageContext;
use crate::error::AgentResult;

pub(crate) fn refresh(ctx: &StageContext) -> AgentResult<bool> {
    let messages = {
        let transcript = ctx.session.transcript.read();
        crate::agent::compact_v2::projection::render_persisted_llm_view(
            &transcript,
            &ctx.runtime.llm.provider_capabilities(),
        )?
    };
    let tools: Vec<_> = ctx
        .runtime
        .tools
        .read()
        .values()
        .filter(|tool| tool.is_direct() && tool.visible_to_model())
        .cloned()
        .collect();
    let tool_refs: Vec<_> = tools.iter().map(|tool| tool.as_ref()).collect();
    let estimate = ctx
        .runtime
        .llm
        .estimate_request_tokens(&messages, &tool_refs);
    Ok(ctx
        .compact
        .token_tracker
        .write()
        .refresh_input_estimate(estimate))
}
