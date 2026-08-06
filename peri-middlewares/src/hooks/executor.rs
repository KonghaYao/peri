use std::{collections::HashSet, process::Stdio, sync::Arc, time::Duration};

use peri_agent::{agent::react::ReactLLM, messages::BaseMessage};
use tokio::io::AsyncWriteExt;

use crate::hooks::{
    output_parser::{parse_command_hook_output, parse_http_hook_response},
    ssrf_guard::check_url,
    types::{HookAction, HookInput, HookType, RegisteredHook},
    variables::resolve_hook_variables,
};

/// Execute a command hook (shell script).
///
/// - shell default "bash", timeout default 600s
/// - stdin: serialized HookInput JSON
/// - exit code 0 → parse stdout, 1 → Allow(warn), 2 → Block(reason)
/// - timeout → Allow(warn)
pub async fn execute_command_hook(
    hook: &HookType,
    input: &HookInput,
    registered: &RegisteredHook,
) -> HookAction {
    let (command, _shell, timeout_secs) = match hook {
        HookType::Command {
            command,
            shell,
            timeout,
            ..
        } => (command.clone(), shell.clone(), timeout.unwrap_or(600)),
        _ => {
            return HookAction::Allow;
        }
    };

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput: {}", e);
            return HookAction::Allow;
        }
    };

    // Resolve ${CLAUDE_PLUGIN_ROOT}, ${CLAUDE_PLUGIN_DATA}, ${ARGUMENTS} in command string
    let command = resolve_hook_variables(
        &command,
        &registered.plugin_root,
        &registered.plugin_data_dir,
        &input_json,
    );

    let plugin_root_str = registered.plugin_root.to_string_lossy().to_string();
    let plugin_data_str = registered.plugin_data_dir.to_string_lossy().to_string();
    let hook_event_str = format!("{:?}", input.hook_event_name);

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut cmd = peri_agent::agent::async_tasks::shell_command(&command, &[]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &input.cwd)
            .env("CLAUDE_PLUGIN_ROOT", &plugin_root_str)
            .env("CLAUDE_PLUGIN_DATA", &plugin_data_str)
            .env("CLAUDE_HOOK_EVENT_NAME", &hook_event_str)
            .kill_on_drop(true);

        // Inject CLAUDE_PLUGIN_OPTION_* env vars
        for (key, value) in &registered.plugin_options {
            let env_key = format!("CLAUDE_PLUGIN_OPTION_{}", key.to_uppercase());
            cmd.env(env_key, value.to_string());
        }

        let mut child = cmd.spawn()?;

        // Write input JSON to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(input_json.as_bytes()).await {
                tracing::warn!("Failed to write to hook stdin: {}", e);
            }
            drop(stdin);
        }

        let output = child.wait_with_output().await?;
        Ok::<_, std::io::Error>(output)
    })
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            match output.status.code() {
                Some(0) => {
                    // Parse structured output
                    parse_command_hook_output(&stdout)
                }
                Some(1) => {
                    // Exit code 1 → Allow with warning
                    if !stderr.is_empty() {
                        tracing::warn!("Command hook exited with code 1: {}", stderr);
                    }
                    HookAction::Allow
                }
                Some(2) => {
                    // Exit code 2 → Block
                    let reason = if !stdout.trim().is_empty() {
                        stdout.trim().to_string()
                    } else if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else {
                        "Blocked by hook (exit code 2)".to_string()
                    };
                    HookAction::Block { reason }
                }
                Some(code) => {
                    tracing::warn!(
                        "Command hook exited with unexpected code {}: stderr={}",
                        code,
                        stderr
                    );
                    HookAction::Allow
                }
                None => {
                    tracing::warn!("Command hook terminated by signal");
                    HookAction::Allow
                }
            }
        }
        Ok(Err(e)) => {
            tracing::warn!("Command hook execution failed: {}", e);
            HookAction::Allow
        }
        Err(_) => {
            // Timeout
            tracing::warn!(
                "Command hook timed out after {}s: {}",
                timeout_secs,
                command
            );
            HookAction::Allow
        }
    }
}

/// Execute a prompt hook (LLM evaluation).
///
/// - timeout default 30s
/// - Replace $ARGUMENTS in prompt with input JSON
/// - Call llm.generate_reasoning, parse result
pub async fn execute_prompt_hook(
    hook: &HookType,
    input: &HookInput,
    llm_factory: &Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
) -> HookAction {
    let (prompt_template, timeout_secs) = match hook {
        HookType::Prompt {
            prompt, timeout, ..
        } => (prompt.as_str(), timeout.unwrap_or(30)),
        _ => {
            return HookAction::Allow;
        }
    };

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for prompt hook: {}", e);
            return HookAction::Allow;
        }
    };

    // Replace $ARGUMENTS with input JSON
    let prompt = prompt_template.replace("$ARGUMENTS", &input_json);
    let prompt = prompt.replace("${ARGUMENTS}", &input_json);

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let llm = llm_factory();
        // Build a minimal message list with just the prompt as a system message
        let messages = vec![BaseMessage::system(prompt.clone())];
        let reasoning = llm.generate_reasoning(&messages, &[], None).await?;
        Ok::<_, anyhow::Error>(reasoning)
    })
    .await;

    match result {
        Ok(Ok(reasoning)) => {
            let answer = reasoning
                .final_answer
                .unwrap_or(reasoning.thought)
                .trim()
                .to_string();
            parse_command_hook_output(&answer)
        }
        Ok(Err(e)) => {
            tracing::warn!("Prompt hook LLM call failed: {}", e);
            HookAction::Allow
        }
        Err(_) => {
            tracing::warn!("Prompt hook timed out after {}s", timeout_secs);
            HookAction::Allow
        }
    }
}

/// Execute an HTTP hook (POST request).
///
/// - SSRF guard check first
/// - timeout default 600s
/// - POST with JSON body, CRLF-injection-safe headers
pub async fn execute_http_hook(hook: &HookType, input: &HookInput) -> HookAction {
    let (url, timeout_secs, headers, allowed_env_vars) = match hook {
        HookType::Http {
            url,
            timeout,
            headers,
            allowed_env_vars,
            ..
        } => (
            url.as_str(),
            timeout.unwrap_or(600),
            headers,
            allowed_env_vars,
        ),
        _ => {
            return HookAction::Allow;
        }
    };

    // SSRF guard
    if let Err(reason) = check_url(url) {
        tracing::warn!("HTTP hook blocked by SSRF guard: {}", reason);
        return HookAction::Block {
            reason: format!("SSRF guard blocked URL: {}", reason),
        };
    }

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for HTTP hook: {}", e);
            return HookAction::Allow;
        }
    };

    // Build allowed_env_vars set for header sanitization
    let allowed_set: HashSet<String> = allowed_env_vars.iter().cloned().collect();

    // Sanitize and build headers
    let mut req_headers = reqwest::header::HeaderMap::new();
    req_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    for (key, value) in headers {
        let sanitized = sanitize_header_value(value, &allowed_set);
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(&sanitized),
        ) {
            req_headers.insert(name, val);
        }
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to build HTTP client: {}", e);
            return HookAction::Allow;
        }
    };

    match client
        .post(url)
        .headers(req_headers)
        .body(input_json)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if !status.is_success() {
                tracing::warn!(
                    "HTTP hook returned non-success status {}: {}",
                    status,
                    if body.len() > 200 {
                        format!("{}...", &body[..body.floor_char_boundary(200)])
                    } else {
                        body
                    }
                );
                return HookAction::Allow;
            }

            parse_http_hook_response(&body)
        }
        Err(e) => {
            tracing::warn!("HTTP hook request failed: {}", e);
            HookAction::Allow
        }
    }
}

/// Execute an agent hook.
///
/// Hook agent 是 1-turn 无工具的 LLM 调用（用 prompt_template + HookInput JSON
/// 作为输入，让 LLM 输出结构化 JSON 表达 Allow/Warn/Block 决策）。无需构造
/// 完整 v2 stages，直接调 `ReactLLM::generate_reasoning` 一次。
///
/// - LLM 输出经 `parse_command_hook_output` 解析为 HookAction
/// - timeout 外层包装（默认 60s）
/// - LLM 失败 / 超时 → Allow（fail-open，与 command hook 一致）
pub async fn execute_agent_hook(
    hook: &HookType,
    input: &HookInput,
    llm_factory: &Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    cwd: &str,
) -> HookAction {
    let (prompt_template, timeout_secs) = match hook {
        HookType::Agent {
            prompt, timeout, ..
        } => (prompt.as_str(), timeout.unwrap_or(60)),
        _ => {
            return HookAction::Allow;
        }
    };

    let input_json = match serde_json::to_string_pretty(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for agent hook: {}", e);
            return HookAction::Allow;
        }
    };

    // 构造 messages：System（prompt_template） + Human（HookInput JSON）
    // Hook agent 期望 LLM 按 prompt_template 指示输出结构化 JSON，parse 阶段提取。
    let messages = vec![
        BaseMessage::system(prompt_template),
        BaseMessage::human(input_json),
    ];

    let llm = llm_factory();
    let _ = cwd; // cwd 保留签名兼容；当前实现未使用（v1 亦仅用于 AgentState::new）

    tracing::debug!(
        timeout_secs,
        "Hook agent: calling LLM directly (1-turn, no tools)"
    );

    // 外层 timeout 包装（与 v1 一致）
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        llm.generate_reasoning(&messages, &[], None),
    )
    .await;

    match result {
        Ok(Ok(reasoning)) => {
            // 优先 final_answer，回落到 source_message
            let text = reasoning
                .final_answer
                .clone()
                .or_else(|| {
                    reasoning
                        .source_message
                        .as_ref()
                        .map(|m| m.content().to_string())
                })
                .unwrap_or_default();
            if text.is_empty() {
                tracing::warn!("Hook agent: LLM returned empty text, allowing");
                return HookAction::Allow;
            }
            // 复用 command hook 的 output parser 解析 JSON 决策
            parse_command_hook_output(&text)
        }
        Ok(Err(e)) => {
            tracing::warn!("Hook agent: LLM failed: {}, allowing", e);
            HookAction::Allow
        }
        Err(_) => {
            tracing::warn!("Hook agent: timed out ({}s), allowing", timeout_secs);
            HookAction::Allow
        }
    }
}

/// Sanitize header value: remove CRLF sequences and expand whitelisted env vars.
///
/// CRLF injection protection: strips \r and \n from header values.
/// Env var expansion: only vars in `allowed_env_vars` set are expanded.
fn sanitize_header_value(value: &str, allowed_env_vars: &HashSet<String>) -> String {
    // First, strip CRLF to prevent injection
    let sanitized = value.replace(['\r', '\n'], "");

    // Expand whitelisted env vars (simple ${VAR} and $VAR patterns)
    let mut result = sanitized;
    for var_name in allowed_env_vars {
        let pattern1 = format!("${{{}}}", var_name);
        let pattern2 = format!("${}", var_name);
        if let Ok(val) = std::env::var(var_name) {
            result = result.replace(&pattern1, &val);
            result = result.replace(&pattern2, &val);
        }
    }

    result
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
