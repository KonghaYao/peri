use peri_acp_types::event_data::PredictionAction;
use peri_acp_types::messages::BaseMessage;
use tracing::debug;

use crate::agent::react::ReactLLM;

// ── Prediction facade ───────────────────────────────────────────────────────

/// 预测失败原因，用于决定是否发送通知及日志级别。
#[derive(Debug)]
pub enum PredictionError {
    /// 30s 超时（首次冷启动可能较慢）。
    Timeout,
    /// Agent 执行返回错误。
    Failed(String),
}

/// Facade：基于现有对话历史预测用户下一步输入。
///
/// 此函数封装了 TUI 之前在 `acp_server/mod.rs` 内联的 Prediction 构造逻辑
/// （`AgentModelBridge::new` + `ReactLLM::generate_reasoning` 一次调用），
/// 避免违反 CLAUDE.md [TRAP]：
///
/// > Agent 构建和执行统一通过会话编排入口 `run_session_loop()`（ACP 侧经
/// > 协议化薄壳 re-export）。禁止在 TUI 层直接构建 Agent。
///
/// 构建一个 1 轮、无工具、无中间件的最小 LLM 调用，注入 `history`（应已过滤 System
/// 消息并限制条数），30 秒超时后返回结构化动作列表或 [`PredictionError`]。
/// 模型输出经 [`parse_prediction_actions`] 解析为 `<peri:xxx>` 标记动作；
/// 无标记时回落为单个 `Placeholder` 动作（现有 placeholder 行为）。
///
/// `current_title` 为会话当前标题（`None` 表示无标题），注入指令后模型才能
/// 判断标题是否需要更新。
///
/// 调用方负责发送 `peri/prediction_ready` 通知（保留在 TUI 层以便复用 transport）。
///
/// L5：LLM 构造（`AgentModelBridge`）由调用方（ACP 宿主）完成——执行体
/// 不引用 ACP provider 类型；指令模板同 crate（`session::subagent`）。
pub async fn execute_prediction(
    llm: Box<dyn ReactLLM + Send + Sync>,
    history: Vec<BaseMessage>,
    cwd: &str,
    current_title: Option<&str>,
) -> Result<Vec<PredictionAction>, PredictionError> {
    debug!(
        msg_count = history.len(),
        cwd, "Prediction facade: starting"
    );

    // execute_prediction 是 1-turn 无工具无中间件的最小 LLM 调用，
    // 不需要构造完整 v2 stages。直接构造 messages 调
    // ReactLLM::generate_reasoning 一次。
    let directive = crate::session::subagent::build_prediction_directive(current_title);
    let mut messages: Vec<BaseMessage> = Vec::with_capacity(history.len() + 2);
    messages.push(BaseMessage::system(directive));
    for msg in &history {
        // 历史 System 已被调用方过滤（仅 Human/Ai/Tool），直接 append
        messages.push(msg.clone());
    }
    messages.push(BaseMessage::human("请根据以上对话预测用户下一步输入"));

    debug!("Prediction facade: calling LLM directly");
    // 30 秒超时（首次冷启动可能较慢）
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        llm.generate_reasoning(&messages, &[], None),
    )
    .await;

    match result {
        Ok(Ok(reasoning)) => {
            // 优先取 final_answer，回落到 source_message 文本
            let text = reasoning
                .final_answer
                .clone()
                .or_else(|| {
                    reasoning
                        .source_message
                        .as_ref()
                        .map(|m| m.content().to_string())
                })
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            if text.is_empty() {
                debug!("Prediction facade: LLM returned empty text");
                Ok(Vec::new())
            } else {
                debug!(%text, "Prediction facade: ready");
                Ok(parse_prediction_actions(&text))
            }
        }
        Ok(Err(e)) => {
            debug!(error = %e, "Prediction facade: LLM failed");
            Err(PredictionError::Failed(e.to_string()))
        }
        Err(_) => {
            debug!("Prediction facade: timed out (30s)");
            Err(PredictionError::Timeout)
        }
    }
}

/// 从 agent 执行后的 state 中提取最后一条非空 AI 消息文本。
///
/// 纯函数（不持有 lock、不 await），便于单元测试。文本两侧空白会被裁剪。
pub fn extract_prediction_text(messages: &[BaseMessage]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 动作内容最大长度（字符数）
const MAX_ACTION_LEN: usize = 200;

/// 解析模型输出为结构化动作列表。
///
/// - 匹配 `<peri:(\w+)>(.*?)</peri:\1>` 标记（非贪婪，取第一个闭合）
/// - 未知标签忽略，其内容并入占位文本流
/// - 标记之间的纯文本片段（trim 后非空）收集为单个 Placeholder
/// - 同名动作后者覆盖前者
/// - 每个动作内容：剥离控制字符（含换行）、trim、截断 200 字符；空内容跳过
/// - 无任何标记/解析失败：整段回落为 Placeholder（现有行为）
pub fn parse_prediction_actions(text: &str) -> Vec<PredictionAction> {
    let mut actions: Vec<PredictionAction> = Vec::new();
    let mut plain_parts: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_open) = text[cursor..].find("<peri:") {
        let open = cursor + rel_open;
        let Some(rel_gt) = text[open..].find('>') else {
            break;
        };
        let tag_end = open + rel_gt;
        let tag = &text[open + "<peri:".len()..tag_end];
        if !tag_is_valid(tag) {
            cursor = open + 1;
            continue;
        }
        let closing = format!("</peri:{tag}>");
        let content_start = tag_end + 1;
        let Some(rel_close) = text[content_start..].find(&closing) else {
            break; // 未闭合：剩余全部按纯文本
        };
        let content_end = content_start + rel_close;
        let whole_tag_end = content_end + closing.len();

        if open > cursor {
            plain_parts.push(text[cursor..open].to_string());
        }
        // 未知标签：标记剥除，仅内容并入占位文本
        if !matches!(tag, "title" | "tag" | "summary") {
            plain_parts.push(text[content_start..content_end].to_string());
            cursor = whole_tag_end;
            continue;
        }
        let action = match tag {
            "title" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::SetTitle { title: content }),
            "tag" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::AddTag { tag: content }),
            "summary" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::Summary { text: content }),
            _ => unreachable!("已知标签已在上面过滤"),
        };
        if let Some(action) = action {
            push_replace_action(&mut actions, action);
        }
        cursor = whole_tag_end;
    }
    if cursor < text.len() {
        plain_parts.push(text[cursor..].to_string());
    }

    let placeholder = plain_parts
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !placeholder.is_empty() {
        actions.insert(0, PredictionAction::Placeholder { text: placeholder });
    }
    actions
}

/// 标签名仅允许 ASCII 字母数字，防止任意闭合注入
fn tag_is_valid(tag: &str) -> bool {
    !tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 剥离控制字符（含换行）、trim、截断；空内容返回 None（跳过动作）
fn sanitize_action_content(s: &str) -> Option<String> {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(MAX_ACTION_LEN).collect())
    }
}

/// 同名（同变体）动作后者覆盖前者
fn push_replace_action(actions: &mut Vec<PredictionAction>, action: PredictionAction) {
    let disc = std::mem::discriminant(&action);
    if let Some(pos) = actions
        .iter()
        .position(|a| std::mem::discriminant(a) == disc)
    {
        actions[pos] = action;
    } else {
        actions.push(action);
    }
}
