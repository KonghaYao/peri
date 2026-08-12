//! ACP Stdio 模式：通过 stdin/stdout JSON-RPC 与 IDE client 通信。
//!
//! stdio host 位于 ACP 层（部署装配点，`docs/top-level.md` §7/§19）；外部
//! 系统通道（thread 存储）由部署单元（cli）打开后经 `thread_store` 注入，
//! ACP 层不直接依赖 Resources（§0 依赖方向）。

mod commands;
mod context;
mod freeze;
mod init;
pub use init::StdioAssemblyInput;
mod model;
mod notification;
mod session;
mod transport;

// ─── run_acp_stdio ───────────────────────────────────────────────────────

/// 启动 ACP stdio 宿主。
///
/// 装配输入（cron/MCP 池/工具检索索引/插件数据等具体实现）由部署装配点
/// （cli 白名单文件，见 `peri-tui/src/main.rs`）构造后经 [`init::StdioAssemblyInput`]
/// 注入；ACP 层只持端口接口（3.0 批 2 波 2，§0 依赖方向）。
pub async fn run_acp_stdio(input: init::StdioAssemblyInput) -> anyhow::Result<()> {
    let ctx = init::init_stdio_context(input).await?;

    use agent_client_protocol::{
        schema::v1::{
            CancelNotification, CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest,
            DeleteSessionResponse, ForkSessionRequest, InitializeRequest, ListSessionsRequest,
            LoadSessionRequest, NewSessionRequest, PromptRequest, ResumeSessionRequest,
            SetSessionConfigOptionRequest, SetSessionModeRequest,
        },
        Agent, Client, ConnectionTo, Stdio,
    };

    let ctx_clone = ctx.clone();

    Agent
        .builder()
        .name("peri-acp")
        // ── initialize ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: InitializeRequest, responder, cx| {
                    transport::handle_initialize(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/new ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                    session::create::handle_new(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/list ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: ListSessionsRequest, responder, _cx: ConnectionTo<Client>| {
                    let resp = session::control::handle_list(&ctx, req).await;
                    let _ = responder.respond(resp);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/prompt ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    session::prompt::handle_prompt(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/set_mode ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: SetSessionModeRequest, responder, cx: ConnectionTo<Client>| {
                    session::config::handle_set_mode(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/set_config_option ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: SetSessionConfigOptionRequest,
                            responder,
                            cx: ConnectionTo<Client>| {
                    session::config::handle_set_config_option(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/cancel ──
        .on_receive_notification(
            {
                let ctx = ctx_clone.clone();
                async move |_notif: CancelNotification, _cx| {
                    session::control::handle_cancel(&ctx, &_notif.session_id.0);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // ── session/close ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: CloseSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    session::control::handle_close(&ctx, &req.session_id.0).await;
                    let _ = responder.respond(CloseSessionResponse::new());
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/resume ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: ResumeSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    session::create::handle_resume(&ctx, req, responder, _cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/load ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                    session::create::handle_load(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/fork ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: ForkSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    session::create::handle_fork(&ctx, req, responder, _cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/delete（标准 ACP：从 session history 移除会话）──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: DeleteSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    session::control::handle_delete(&ctx, &req.session_id.0).await;
                    let _ = responder.respond(DeleteSessionResponse::new());
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ── session/update_config (custom extension) ──
        .on_receive_request(
            {
                let ctx = ctx_clone.clone();
                async move |req: agent_client_protocol::UntypedMessage,
                            responder,
                            cx: ConnectionTo<Client>| {
                    session::config::handle_update_config(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new().with_debug(transport::cancel_debug_hook(ctx_clone.clone())))
        .await
        .map_err(|e| anyhow::anyhow!("ACP error: {e}"))?;

    // ── 宿主退出：优雅关闭所有会话的 LSP 服务器池（H1 shutdown 钩子）──
    // stdin EOF / 传输关闭 = 宿主退出。sessions 即将 drop；此处显式 shutdown，
    // 避免 LSP 服务器子进程随进程残留。（先收集端口再 await，不跨 await 持锁）
    let lsp_pools: Vec<_> = ctx
        .sessions
        .read()
        .values()
        .filter_map(|info| info.lsp_pool.clone())
        .collect();
    for pool in lsp_pools {
        pool.shutdown().await;
    }
    Ok(())
}
