//! -p/--print 非交互模式：经 ACP ephemeral session 单轮问答后自动退出。
//!
//! 3.0 归位（`docs/top-level.md` §8）：print = 同层轻量渲染客户端（无界面，
//! 输出文本），经 ACP 走 ephemeral session（不造 sessionless）。本模块不再
//! 直连 `run_session_loop`；host 装配复用 `peri_acp::host::assemble`（与 TUI
//! 同源，不复制），执行路径与 TUI 完全一致（session/new → prompt → 事件收集
//! → close）。

use std::path::PathBuf;
use std::sync::Arc;

use crate::cli_args::OutputFormat;
use agent_client_protocol::schema::v1::StopReason;
use anyhow::Result;
use peri_acp::host::assemble::{HostAssemblyInput, assemble_server_config};
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_acp_types::interaction::UnansweredCause;
use peri_acp_types::messages::MessageContent;
use peri_tui::acp_client::{
    AcpDeployment, AcpNotification, AcpTuiClient,
    interaction_response::{
        elicitation_unanswered_response, permission_selected_allow_once_response,
    },
};
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
    db_path: Option<PathBuf>,
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

    // 加载配置（经 ConfigSource 承载读写路径决策，与 TUI 同源；print 模式
    // 无保存，但装配面需要一致的来源语义）：
    // - --settings：指定文件整体生效（单文件来源，不合并全局/工作区）
    // - 默认：全局 + 工作区分层合并（load_lenient 保持迁移前
    //   `load().unwrap_or_default()` 的容错语义）
    let config_source = match &settings_path {
        Some(path) => {
            let p = std::path::Path::new(path);
            if p.exists() {
                Arc::new(peri_tui::config::ConfigSource::load_standalone(
                    p.to_path_buf(),
                )?)
            } else {
                let v: serde_json::Value = serde_json::from_str(path)
                    .map_err(|e| anyhow::anyhow!("--settings 不是有效文件路径或 JSON: {e}"))?;
                let tmp = std::env::temp_dir().join("peri-settings-override.json");
                std::fs::write(&tmp, serde_json::to_string_pretty(&v)?)?;
                Arc::new(peri_tui::config::ConfigSource::load_standalone(tmp)?)
            }
        }
        None => Arc::new(peri_tui::config::ConfigSource::load_lenient()),
    };
    let peri_config = config_source.loaded_merged();

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
    // 默认或显式 db_path 打开失败都经 `?` 传播 exit 1。
    let thread_store = peri_resources::Resources::open_with(db_path)
        .await
        .map(|resources| resources.thread_store())
        .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;

    // ── ACP host 装配（与 TUI 同源，见 peri_acp::host::assemble）──
    let host_config = assemble_server_config(HostAssemblyInput {
        provider: provider.clone(),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        config_source: config_source.clone(),
        permission_mode: shared_permission,
        thread_store: thread_store.clone(),
        cwd: cwd.clone(),
        bare,
        // print 无 tick 语义（迁移前 print 路径无每秒 tick，行为零变化）。
        drive_cron_tick: false,
    })
    .await;
    let (client_transport, server_transport) = mpsc_transport_pair();
    let host = peri_acp::host::spawn_acp_server(Arc::new(server_transport), host_config);

    let (acp_client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    acp_client.spawn_pump(notification_tx);

    let mut deployment = AcpDeployment::new(acp_client.clone(), host);
    let mut output = PrintOutput::new(fmt);
    let mut stop_reason = None;
    let operation = async {
        // ── ephemeral session：new → prompt（流式收集事件）→ close ──
        let session_id = acp_client.new_session(&cwd, None).await?;

        {
            // prompt future 借用 acp_client，收敛在块内以便之后 drop(client)
            let content = MessageContent::text(prompt_text);
            let prompt_fut = acp_client.prompt_with_response(&content, None);
            tokio::pin!(prompt_fut);

            loop {
                tokio::select! {
                    biased;
                    res = &mut prompt_fut => {
                        let response = res.map_err(|e| anyhow::anyhow!("session/prompt 失败: {e}"))?;
                        stop_reason = Some(response.stop_reason);
                        break;
                    }
                    notif = notification_rx.recv() => {
                        if !consume_print_notification(&acp_client, &mut output, notif).await {
                            anyhow::bail!("session/prompt notification channel closed before terminal response");
                        }
                    }
                }
            }
        }

        // 关闭 ephemeral session（释放 host 侧 history/frozen/agent_pool）
        acp_client
            .send_raw_request("session/close", json!({ "sessionId": session_id }))
            .await
            .map_err(|_| anyhow::anyhow!("ACP session cleanup failed"))?;
        Ok(())
    };
    let cleanup_result = deployment.run(operation).await;
    // transport close 会唤醒 pump；它独占 notification sender，recv(None)
    // 才证明已经消费全部尾帧，某一时刻 is_empty 不能证明通知已结束。
    drain_print_notifications(&acp_client, &mut output, &mut notification_rx).await;
    if let Some(stop_reason) = stop_reason {
        // result 只在 deployment 已结束后发出；任务终态与清理失败分别表达。
        let cleanup_error = cleanup_result.as_ref().err().map(|_| "ACP cleanup failed");
        output.output_final(stop_reason, cleanup_error);
        cleanup_result?;
        require_completed(stop_reason)
    } else {
        cleanup_result?;
        anyhow::bail!("session/prompt ended without a terminal response")
    }
}

fn require_completed(stop_reason: StopReason) -> Result<()> {
    if stop_reason == StopReason::EndTurn {
        Ok(())
    } else {
        anyhow::bail!("session/prompt incomplete: {}", json!(stop_reason))
    }
}

async fn drain_print_notifications(
    acp_client: &AcpTuiClient,
    output: &mut PrintOutput,
    notification_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AcpNotification>,
) {
    while let Some(notification) = notification_rx.recv().await {
        consume_print_notification(acp_client, output, Some(notification)).await;
    }
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
        AcpNotification::RequestPermission { owner, params, .. } => {
            auto_approve(acp_client, owner, &params).await;
        }
        AcpNotification::Elicitation { owner, .. } => {
            let _ = acp_client
                .respond_interaction(
                    &owner,
                    print_elicitation_response(),
                    "Cancelled".to_string(),
                )
                .await;
        }
        _ => {}
    }
    true
}

/// 自动批准所有权限请求（等价迁移前 `PrintBroker` 语义：-p 模式无交互）。
async fn auto_approve(
    client: &AcpTuiClient,
    owner: peri_tui::acp_client::InteractionOwner,
    params: &Value,
) {
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let tool_name = tool_call
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    tracing::info!(tool = %tool_name, "print mode: auto-approving permission request");
    let response = print_permission_response();
    if let Err(e) = client
        .respond_interaction(&owner, response, "Allowed once".to_string())
        .await
    {
        tracing::warn!(error = %e, "print mode: auto-approve send_response failed");
    }
}

fn print_permission_response() -> Value {
    permission_selected_allow_once_response()
}

/// `-p` 无交互界面：声明「无人可作答」而非裸 cancel，工具据此如实转述。
fn print_elicitation_response() -> Value {
    elicitation_unanswered_response(UnansweredCause::NonInteractiveClient)
}

/// 事件输出器：消费 ACP 协议化事件（session/update 通知），输出格式与
/// 迁移前 `PrintCollector`（ExecutorEvent 直连）保持一致。
struct PrintOutput {
    fmt: OutputFormat,
    text_buffer: String,
    total_usage: Option<PrintUsage>,
    usage_count: u64,
}

// ACP inputTokens 已含缓存；Claude 形状的三个 input 字段互不重叠。
#[derive(Default, serde::Serialize)]
struct PrintUsage {
    input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    output_tokens: u64,
}

impl PrintUsage {
    fn from_meta(meta: &Value) -> Option<Self> {
        let total_input = meta.get("inputTokens")?.as_u64()?;
        let cache_read_input_tokens = meta.get("cacheReadTokens").map_or(Some(0), Value::as_u64)?;
        let cache_creation_input_tokens = meta
            .get("cacheCreationTokens")
            .map_or(Some(0), Value::as_u64)?;
        Some(Self {
            input_tokens: total_input
                .checked_sub(cache_read_input_tokens)?
                .checked_sub(cache_creation_input_tokens)?,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            output_tokens: meta.get("outputTokens")?.as_u64()?,
        })
    }

    fn add(&mut self, usage: &Self) {
        self.input_tokens += usage.input_tokens;
        self.cache_read_input_tokens += usage.cache_read_input_tokens;
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.output_tokens += usage.output_tokens;
    }
}

impl PrintOutput {
    fn new(fmt: OutputFormat) -> Self {
        Self {
            fmt,
            text_buffer: String::new(),
            total_usage: None,
            usage_count: 0,
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
            "usage_update" if self.fmt == OutputFormat::StreamJson => {
                let meta = update.get("_meta")?;
                if meta.get("periReplay").and_then(Value::as_bool) == Some(true) {
                    return None;
                }
                let usage = PrintUsage::from_meta(meta)?;
                self.total_usage
                    .get_or_insert_with(PrintUsage::default)
                    .add(&usage);
                self.usage_count += 1;
                // 每条 canonical LlmCallEnd 只投影一次；独立生成消息 ID，避免
                // 网关缺失或复用 requestId 时评测端按 ID 去重丢失不同调用。
                Some(
                    json!({
                        "type": "assistant",
                        "message": {
                            "id": format!("msg_peri_{}", self.usage_count),
                            "usage": usage,
                        },
                    })
                    .to_string(),
                )
            }
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

    fn output_final(&self, stop_reason: StopReason, cleanup_error: Option<&str>) {
        match self.fmt {
            OutputFormat::Text => println!("{}", self.text_buffer),
            OutputFormat::Json => println!(
                "{}",
                serde_json::to_string_pretty(&self.result(stop_reason, cleanup_error)).unwrap()
            ),
            OutputFormat::StreamJson => println!("{}", self.result(stop_reason, cleanup_error)),
        }
    }

    fn result(&self, stop_reason: StopReason, cleanup_error: Option<&str>) -> Value {
        let completed = stop_reason == StopReason::EndTurn;
        let mut result = json!({
            "type": "result",
            "stop_reason": stop_reason,
            "status": if completed { "completed" } else { "incomplete" },
            "is_error": !completed || cleanup_error.is_some(),
        });
        match self.fmt {
            OutputFormat::StreamJson => {
                // 没有 provider usage / pricing 时保留未知，不伪装成零消耗。
                result["usage"] = json!(self.total_usage);
                result["total_cost_usd"] = Value::Null;
            }
            OutputFormat::Text | OutputFormat::Json => {
                result["content"] = json!(self.text_buffer);
            }
        }
        if let Some(error) = cleanup_error {
            result["cleanup_error"] = json!(error);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{RequestPermissionOutcome, RequestPermissionResponse};
    use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};

    use super::*;

    /// [回归测试] 临时空队列之后才到达的尾帧仍须进入最终输出。
    #[tokio::test]
    async fn test_print_drain_waits_for_sender_close_and_keeps_tail_events() {
        for fmt in [OutputFormat::Json, OutputFormat::StreamJson] {
            let (transport, _server) = mpsc_transport_pair();
            let (client, sender, mut receiver) = AcpTuiClient::new(transport);
            let mut output = PrintOutput::new(fmt);
            {
                let drain = drain_print_notifications(&client, &mut output, &mut receiver);
                tokio::pin!(drain);
                tokio::select! {
                    biased;
                    () = &mut drain => panic!("sender 尚存时不能把暂时空队列当作结束"),
                    () = std::future::ready(()) => {}
                }
                for update in [
                    json!({"sessionUpdate": "agent_message_chunk", "content": {"text": "tail"}}),
                    json!({"sessionUpdate": "usage_update", "_meta": {"inputTokens": 42, "outputTokens": 7}}),
                ] {
                    sender
                        .send(AcpNotification::SessionUpdate {
                            session_id: "s1".into(),
                            params: json!({"sessionId": "s1", "update": update}),
                        })
                        .unwrap();
                }
                drop(sender);
                drain.await;
            }
            let result = output.result(StopReason::EndTurn, None);
            if fmt == OutputFormat::Json {
                assert_eq!(result["content"], "tail");
            } else {
                assert_eq!(result["usage"]["input_tokens"], 42);
                assert_eq!(result["usage"]["output_tokens"], 7);
            }
            client.close();
        }
    }

    /// [回归测试] ACP 成功响应中的非成功终态不可投影为成功 result。
    #[test]
    fn test_print_result_preserves_incomplete_stop_reasons() {
        for stop_reason in [
            StopReason::Cancelled,
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
        ] {
            for fmt in [OutputFormat::Json, OutputFormat::StreamJson] {
                let output = PrintOutput::new(fmt);
                let result = output.result(stop_reason, None);
                assert_eq!(result["stop_reason"], json!(stop_reason));
                assert_eq!(result["status"], "incomplete");
                assert_eq!(result["is_error"], true);
                assert!(require_completed(stop_reason).is_err());
            }
        }
    }

    #[test]
    fn test_print_result_distinguishes_completed_task_from_cleanup_failure() {
        let output = PrintOutput::new(OutputFormat::Json);
        let result = output.result(StopReason::EndTurn, Some("ACP cleanup failed"));
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["is_error"], true);
        assert_eq!(result["cleanup_error"], "ACP cleanup failed");
        assert!(require_completed(StopReason::EndTurn).is_ok());
    }

    #[test]
    fn test_print_permission_response_is_selected_allow_once() {
        let response: RequestPermissionResponse =
            serde_json::from_value(print_permission_response()).unwrap();
        let RequestPermissionOutcome::Selected(selected) = response.outcome else {
            panic!("print permission 应自动选择 allow_once")
        };
        assert_eq!(selected.option_id.0.as_ref(), "allow_once");
    }

    /// [回归测试] `-p` 取消 elicitation 时必须声明「无人可作答」，否则 broker
    /// 只能把它当成裸 cancel → 空答案，模型会把伪造的空回答当真。
    #[test]
    fn test_print_elicitation_response_declares_non_interactive_client() {
        let response: CreateElicitationResponse =
            serde_json::from_value(print_elicitation_response()).unwrap();
        assert!(matches!(response.action, ElicitationAction::Cancel));
        assert_eq!(
            UnansweredCause::from_meta(response.meta.as_ref()),
            Some(UnansweredCause::NonInteractiveClient)
        );
    }
}

#[cfg(test)]
#[path = "cli_print_usage_test.rs"]
mod usage_tests;
