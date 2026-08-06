//! -p/--print 非交互模式：经 ACP ephemeral session 单轮问答后自动退出。
//!
//! 3.0 归位（`docs/top-level.md` §8）：print = 同层轻量渲染客户端（无界面，
//! 输出文本），经 ACP 走 ephemeral session（不造 sessionless）。本模块不再
//! 直连 `run_session_loop`；host 装配复用 `peri_acp::host::assemble`（与 TUI
//! 同源，不复制），执行路径与 TUI 完全一致（session/new → prompt → 事件收集
//! → close）。

use std::sync::Arc;

use crate::cli_args::OutputFormat;
use anyhow::Result;
use peri_acp::LangfuseSessionLike;
use peri_acp::host::assemble::{HostAssemblyInput, assemble_server_config};
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_acp_types::messages::MessageContent;
use peri_tui::acp_client::{AcpNotification, AcpTuiClient};
use serde_json::{Value, json};

/// -p 模式执行入口
#[allow(clippy::too_many_arguments)]
pub async fn run_print(
    prompt: Option<String>,
    output_format: Option<String>,
    max_turns: Option<u32>,
    bare: bool,
    model_override: Option<String>,
    effort_override: Option<String>,
    permission_mode_str: Option<String>,
    skip_permissions: bool,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    settings_path: Option<String>,
    cwd: Option<String>,
) -> Result<()> {
    let fmt: OutputFormat = match output_format.as_deref() {
        Some(s) => s.parse().map_err(|e: String| anyhow::anyhow!(e))?,
        None => OutputFormat::Text,
    };

    let prompt_text = match prompt {
        Some(p) => p,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim().to_string()
        }
    };

    if prompt_text.is_empty() {
        anyhow::bail!("无输入 prompt。用法: peri -p \"你的问题\" 或 echo \"问题\" | peri -p");
    }

    let _telemetry = peri_acp::telemetry::init_tracing("peri-print");

    // 加载配置
    let peri_config = match &settings_path {
        Some(path) => {
            let p = std::path::Path::new(path);
            if p.exists() {
                peri_tui::config::load_from(p)?
            } else {
                let v: serde_json::Value = serde_json::from_str(path)
                    .map_err(|e| anyhow::anyhow!("--settings 不是有效文件路径或 JSON: {e}"))?;
                let tmp = std::env::temp_dir().join("peri-settings-override.json");
                std::fs::write(&tmp, serde_json::to_string_pretty(&v)?)?;
                peri_tui::config::load_from(&tmp)?
            }
        }
        None => peri_tui::config::load().unwrap_or_default(),
    };

    // 构建 provider
    let provider = peri_tui::app::agent::LlmProvider::from_config(&peri_config)
        .or_else(peri_tui::app::agent::LlmProvider::from_env)
        .ok_or_else(|| {
            anyhow::anyhow!("未配置 LLM provider。请设置 ANTHROPIC_API_KEY 或 OPENAI_API_KEY")
        })?;

    // --model 覆盖
    let provider = if let Some(ref model_str) = model_override {
        peri_tui::app::agent::LlmProvider::from_config_for_alias(&peri_config, model_str)
            .unwrap_or(provider)
    } else {
        provider
    };

    let _ = (effort_override, max_turns, allowed_tools, disallowed_tools);

    let cwd = cwd
        .as_deref()
        .map(|c| std::path::Path::new(c).canonicalize())
        .transpose()?
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .to_string_lossy()
        .to_string();

    tracing::info!(
        provider = %provider.display_name(),
        model = %provider.model_name(),
        cwd = %cwd,
        output = ?fmt,
        "print mode starting"
    );

    // 权限模式（-p 默认 bypass）
    let permission_mode = if skip_permissions {
        peri_acp_types::permission::PermissionMode::Bypass
    } else if let Some(ref mode_str) = permission_mode_str {
        match mode_str.as_str() {
            "bypass" => peri_acp_types::permission::PermissionMode::Bypass,
            "default" => peri_acp_types::permission::PermissionMode::Default,
            "accept-edit" => peri_acp_types::permission::PermissionMode::AcceptEdit,
            "auto-mode" => peri_acp_types::permission::PermissionMode::AutoMode,
            _ => peri_acp_types::permission::PermissionMode::Bypass,
        }
    } else {
        peri_acp_types::permission::PermissionMode::Bypass
    };
    let shared_permission = peri_acp_types::permission::SharedPermissionMode::new(permission_mode);

    // thread 存储（经 Resources 门面）——协议面输入，ACP host 的 ephemeral
    // session 需要；middlewares 具体实现（CronScheduler / McpClientPool / 插件
    // 数据等）由 ACP Host 装配面内部构造（§0 依赖方向）。
    let thread_store = peri_resources::Resources::open()
        .await
        .map(|resources| resources.thread_store())
        .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;

    // ── ACP host 装配（与 TUI 同源，见 peri_acp::host::assemble）──
    let host_config = assemble_server_config(HostAssemblyInput {
        provider: provider.clone(),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: shared_permission,
        thread_store: thread_store.clone(),
        cwd: cwd.clone(),
        bare,
        // print 无 tick 语义（迁移前 print 路径无每秒 tick，行为零变化）。
        drive_cron_tick: false,
    })
    .await;
    // Langfuse 句柄先行 clone：host_config 将 move 进 host task，
    // 退出前 flush 仍需引用（短生命周期进程语义，见下方冲刷点）。
    let langfuse_session = host_config.langfuse_session.clone();

    let (client_transport, server_transport) = mpsc_transport_pair();
    let host_task = tokio::spawn(async move {
        peri_acp::host::run_acp_server(Arc::new(server_transport), host_config).await;
    });

    let (acp_client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    acp_client.spawn_pump(notification_tx);

    // ── ephemeral session：new → prompt（流式收集事件）→ close ──
    let session_id = acp_client.new_session(&cwd, None).await?;

    let mut output = PrintOutput::new(fmt);
    {
        // prompt future 借用 acp_client，收敛在块内以便之后 drop(client)
        let content = MessageContent::text(prompt_text);
        let prompt_fut = acp_client.prompt(&content, None);
        tokio::pin!(prompt_fut);

        let mut prompt_returned = false;
        loop {
            if !prompt_returned {
                tokio::select! {
                    res = &mut prompt_fut => {
                        res.map_err(|e| anyhow::anyhow!("session/prompt 失败: {e}"))?;
                        prompt_returned = true;
                    }
                    notif = notification_rx.recv() => {
                        if !consume_print_notification(&acp_client, &mut output, notif).await {
                            break;
                        }
                    }
                }
            } else {
                // prompt 响应已返回：turn 已结束，drain 尾部事件后退出
                match notification_rx.recv().await {
                    Some(notif) => {
                        if !consume_print_notification(&acp_client, &mut output, Some(notif)).await
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
            if prompt_returned && notification_rx.is_empty() {
                break;
            }
        }
    }

    output.output_final();

    // 关闭 ephemeral session（释放 host 侧 history/frozen/agent_pool）
    let _ = acp_client
        .send_raw_request("session/close", json!({ "sessionId": session_id }))
        .await;
    drop(acp_client); // drop transport → host loop 退出

    // 短生命周期进程冲刷：Langfuse 事件在 host 侧收集，退出前显式 flush
    // （fire-and-forget 的 flush task 会随进程退出被 abort，导致 trace 丢失）。
    if let Some(session) = &langfuse_session {
        match session.flush().await {
            Ok(()) => tracing::info!("Langfuse trace flushed before exit (print mode)"),
            Err(e) => tracing::warn!(error = %e, "langfuse: print 模式退出前 flush 失败"),
        }
    }

    let _ = host_task.await;
    Ok(())
}

/// 消费一条 ACP 通知：流式输出 / 自动批准 / 忽略。返回 `false` 表示通道关闭。
async fn consume_print_notification(
    acp_client: &AcpTuiClient,
    output: &mut PrintOutput,
    notif: Option<AcpNotification>,
) -> bool {
    let Some(notif) = notif else {
        return false;
    };
    match notif {
        AcpNotification::SessionUpdate { params, .. } => {
            if let Some(line) = output.handle_session_update(&params) {
                println!("{line}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
        AcpNotification::RequestPermission { id, params } => {
            auto_approve(acp_client, id, &params).await;
        }
        AcpNotification::Elicitation { id, .. } => {
            let _ = acp_client
                .send_response(id, Ok(json!({"answers": []})))
                .await;
        }
        _ => {}
    }
    true
}

/// 自动批准所有权限请求（等价迁移前 `PrintBroker` 语义：-p 模式无交互）。
async fn auto_approve(
    client: &AcpTuiClient,
    id: peri_acp::transport::types::RequestId,
    params: &Value,
) {
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let tool_name = tool_call
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    tracing::info!(tool = %tool_name, "print mode: auto-approving permission request");
    let response = json!({
        "action": "accept",
        "content": Value::Null,
    });
    if let Err(e) = client.send_response(id, Ok(response)).await {
        tracing::warn!(error = %e, "print mode: auto-approve send_response failed");
    }
}

/// 事件输出器：消费 ACP 协议化事件（session/update 通知），输出格式与
/// 迁移前 `PrintCollector`（ExecutorEvent 直连）保持一致。
struct PrintOutput {
    fmt: OutputFormat,
    text_buffer: String,
}

impl PrintOutput {
    fn new(fmt: OutputFormat) -> Self {
        Self {
            fmt,
            text_buffer: String::new(),
        }
    }

    /// 处理一条 `session/update` 通知，返回需要立即输出的行（None = 无输出）。
    ///
    /// 提取逻辑与 TUI `kit/acp_notifier.rs` 一致（同一协议化事实源）：
    /// `params.update` 携带 `sessionUpdate` tag；流式 tag 为
    /// `agent_message_chunk` / `tool_call` / `tool_call_update`。
    fn handle_session_update(&mut self, params: &Value) -> Option<String> {
        let update = params.get("update")?;
        let tag = update.get("sessionUpdate").and_then(|v| v.as_str())?;
        match tag {
            "agent_message_chunk" => {
                let text = update
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.fmt {
                    OutputFormat::StreamJson => Some(
                        serde_json::to_string(&serde_json::json!({
                            "type": "text",
                            "content": text
                        }))
                        .unwrap(),
                    ),
                    OutputFormat::Text | OutputFormat::Json => {
                        self.text_buffer.push_str(&text);
                        None
                    }
                }
            }
            "tool_call" => {
                if self.fmt == OutputFormat::StreamJson {
                    let tool_call_id = update
                        .get("toolCallId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // ACP SDK ToolCall 使用 "title" 字段，而非 "name"
                    let name = update
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
                    Some(
                        serde_json::to_string(&serde_json::json!({
                            "type": "tool_use",
                            "id": tool_call_id,
                            "name": name,
                            "input": input,
                        }))
                        .unwrap(),
                    )
                } else {
                    None
                }
            }
            "tool_call_update" => {
                if self.fmt == OutputFormat::StreamJson {
                    let tool_call_id = update
                        .get("toolCallId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let status = update
                        .get("status")
                        .or_else(|| update.get("fields").and_then(|f| f.get("status")))
                        .and_then(|v| v.as_str());
                    // 仅 completed/failed 视为工具结果；其余状态（in_progress 等）跳过
                    let _ = status.filter(|s| matches!(*s, "completed" | "failed"))?;
                    let output = update
                        .get("rawOutput")
                        .or_else(|| update.get("fields").and_then(|f| f.get("rawOutput")));
                    let output = match output {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) => serde_json::to_string(v).unwrap_or_default(),
                        None => String::new(),
                    };
                    Some(
                        serde_json::to_string(&serde_json::json!({
                            "type": "tool_result",
                            "id": tool_call_id,
                            "output": output,
                        }))
                        .unwrap(),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn output_final(&self) {
        match self.fmt {
            OutputFormat::Text => {
                println!("{}", self.text_buffer);
            }
            OutputFormat::Json => {
                let result = serde_json::json!({
                    "type": "result",
                    "content": self.text_buffer,
                });
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            }
            OutputFormat::StreamJson => {}
        }
    }
}
