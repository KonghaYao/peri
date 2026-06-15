//! ACP Stdio 模式：通过 stdin/stdout JSON-RPC 与 IDE client 通信

mod commands;
mod context;
mod freeze;
mod init;
mod model;
mod notification;
mod session;
mod transport;

// ─── run_acp_stdio ───────────────────────────────────────────────────────

pub async fn run_acp_stdio(cwd: String) -> anyhow::Result<()> {
    let ctx = init::init_stdio_context(cwd).await?;

    use agent_client_protocol::{
        schema::{
            CancelNotification, CloseSessionRequest, CloseSessionResponse, ForkSessionRequest,
            InitializeRequest, ListSessionsRequest, LoadSessionRequest, NewSessionRequest,
            PromptRequest, ResumeSessionRequest, SetSessionConfigOptionRequest,
            SetSessionModeRequest,
        },
        Agent, Client, ConnectionTo, Stdio,
    };

    let ctx_clone = ctx.clone();

    Agent
        .builder()
        .name("peri-acp")
        // ── initialize ──
        .on_receive_request(
            async move |req: InitializeRequest, responder, cx| {
                transport::handle_initialize(req, responder, cx).await
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
        .map_err(|e| anyhow::anyhow!("ACP error: {e}"))
}
