//! Prompt 入口：参数转换 + tokio::spawn 调度。

use std::sync::Arc;

use agent_client_protocol::{
    schema::{PromptRequest, PromptResponse, StopReason},
    Client, ConnectionTo, Responder,
};
use peri_agent::{
    agent::AgentCancellationToken, messages::ContentBlock as PeriContentBlock,
    messages::MessageContent,
};

use super::super::context::StdioContext;
use super::prompt_exec::{self, PromptExecParams};

/// session/prompt 处理器（薄入口）。
///
/// 内容转换、捕获会话数据、设置取消令牌、提取 AgentPool 后，
/// 通过 `tokio::spawn` 将重活转交 `prompt_exec::run()`，
/// 保持事件循环对 session/cancel 的响应性。
///
/// 接收 `&Arc<StdioContext>` 以便 `Arc::clone` 进入后台任务。
pub(crate) async fn handle_prompt(
    ctx: &Arc<StdioContext>,
    req: PromptRequest,
    responder: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    // Convert ACP SDK ContentBlocks to peri-agent MessageContent
    let content = if req.prompt.is_empty() {
        MessageContent::text("")
    } else {
        let blocks: Vec<PeriContentBlock> = req
            .prompt
            .iter()
            .filter_map(|b| match b {
                agent_client_protocol::schema::ContentBlock::Text(t) => {
                    Some(PeriContentBlock::text(&t.text))
                }
                agent_client_protocol::schema::ContentBlock::Image(img) => {
                    Some(PeriContentBlock::image_base64(&img.mime_type, &img.data))
                }
                _ => None, // Audio/ResourceLink/Resource not supported yet
            })
            .collect();
        if blocks.is_empty() {
            MessageContent::text("")
        } else {
            MessageContent::Blocks(blocks)
        }
    };

    // --- capture session-scoped data under the read lock ---
    let (agent_cwd, history, session_start_source, thread_id, frozen) = {
        let sessions = ctx.sessions.read();
        match sessions.get(&sid) {
            Some(s) => (
                s.cwd.clone(),
                s.history.clone(),
                if s.history.is_empty() {
                    Some("startup".to_string())
                } else {
                    None
                },
                s.thread_id.clone(),
                s.frozen.clone(),
            ),
            None => {
                let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
                return Ok(());
            }
        }
    };
    let history_len = history.len();

    let cancel = AgentCancellationToken::new();
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            s.cancel_token = Some(cancel.clone());
        }
    }

    // Extract AgentPool from session for cross-prompt LLM reuse
    let pool_arc = {
        let mut sessions = ctx.sessions.write();
        let pool = sessions
            .get_mut(&sid)
            .map(|s| {
                std::mem::replace(
                    &mut s.agent_pool,
                    peri_acp::session::agent_pool::AgentPool::new(),
                )
            })
            .unwrap_or_default();
        Arc::new(parking_lot::Mutex::new(pool))
    };

    // --- capture everything the background task needs ---
    let ctx_for_task = Arc::clone(ctx);
    let cx_for_task = cx.clone();
    let session_id = req.session_id.clone();

    // Spawn the heavy work to keep the event loop responsive.
    // responder is moved into the task; the response is sent
    // when execution completes (or is cancelled).
    tokio::spawn(async move {
        let params = PromptExecParams {
            ctx: ctx_for_task,
            cx: cx_for_task,
            session_id,
            sid,
            agent_cwd,
            content,
            frozen,
            history,
            session_start_source,
            history_len,
            cancel,
            pool: pool_arc,
            thread_id,
            responder,
        };
        prompt_exec::run(params).await;
    });

    // Return immediately — the event loop stays free to
    // process session/cancel and {"type":"cancel"}.
    Ok(())
}
