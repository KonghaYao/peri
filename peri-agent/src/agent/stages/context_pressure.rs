//! 以实际可见请求视图补齐 provider usage 之间的增长及冷启动预算。

use crate::agent::stages::StageContext;
use crate::error::AgentResult;

pub(crate) fn refresh(ctx: &StageContext) -> AgentResult<bool> {
    // 视图只依赖 transcript 内容与 provider 能力：修订未变时复用上一次渲染结果。
    let messages = {
        let transcript = ctx.session.transcript.read();
        let capabilities = ctx.runtime.llm.provider_capabilities();
        ctx.model_view.view(&transcript, &capabilities)?
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
