//! Tolerant invocation decoding and live parent-message selection.
use peri_agent::messages::BaseMessage;
use peri_agent::tools::{EffectiveToolError, EffectiveToolErrorCode};

pub(super) struct InvocationArgs {
    pub(super) resume_thread_id: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) subagent_type: Option<String>,
    pub(super) model: Option<String>,
    /// **显式**意图（`None` = 调用未给出该字段）：缺省由装配期注入的有效缺省补齐。
    ///
    /// 显式与缺省必须分开承载：只有显式 `true` 会与「后台能力未装配」的场景
    /// （MCP Agent / resume）冲突，缺省不得把它们推进报错路径。
    pub(super) run_in_background: Option<bool>,
    pub(super) cwd: Option<String>,
    pub(super) is_fork: bool,
}

impl InvocationArgs {
    /// 解析工具入参；`resume_thread_id` 非空但非法（非 UUID）时返回明确错误。
    ///
    /// `result_large_err`：本函数对外只剩 `Box<dyn Error>`（调用方看不到具体 Err 类型），
    /// lint 只是看穿了 `?` 的装箱转换。装箱由调用方完成，这里不额外包装；债务归属契约层
    /// ——`EffectiveToolError` 携带 `RemoteToolError` 后变大，若后续再触发该 lint，
    /// 应在契约层决定是否装箱，而不是在本函数堆多层 allow。
    #[allow(clippy::result_large_err)]
    pub(super) fn parse(
        input: &serde_json::Value,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // 未实现的隔离必须在任何 spawn/resume/fork 副作用之前拒绝。
        if input.get("isolation").is_some_and(|value| !value.is_null()) {
            return Err(EffectiveToolError::new(
                EffectiveToolErrorCode::InvalidInput,
                "isolation is not supported: the Agent tool does not create worktrees; remove the isolation parameter",
            ).into());
        }
        // resume_thread_id 的三分语义（「不填 = 新建」）：
        // - 字段缺失 / 非字符串 / 空串 → 未提供，走新建路径（LLM 表达「省略」时常用
        //   ""；它等同未填，不得被当成恢复意图；错误类型的宽容解码与 model / cwd /
        //   fork 等相邻字段一致）；
        // - 合法 UUID → 恢复该 child thread（spawn 时 `Uuid::now_v7` 生成）；
        // - 其它非空字符串 → 明确报错，不得静默忽略该字段。
        //
        // 后台 task_id 形如 `bg-{uuid}`（`session/subagent/factory/spawn.rs` 生成），
        // 与 child_thread_id 同属 id 形状：静默忽略会让一次「恢复」调用转入新建
        // 路径——同一任务被重复执行并造成工作区竞争，因此必须显式失败而非降级。
        let resume_thread_id = input
            .get("resume_thread_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                if uuid::Uuid::parse_str(s).is_ok() {
                    Ok(s.to_string())
                } else {
                    Err(invalid_resume_thread_id(s))
                }
            })
            .transpose()?;
        // prompt 改为 Option：resume 路径可缺省（缺省注入隐式 continue，issue 决策 9）；
        // 非 resume 路径下方运行时校验兜底（required:[] 后语义不变）
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let subagent_type = input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // model 档位覆盖（仅新建定义型 subagent 消费；fork/resume 路径忽略，
        // 与 resume 忽略 subagent_type/fork 的宽容语义一致）
        let model = input
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let run_in_background = input.get("run_in_background").and_then(|v| v.as_bool());
        let cwd = input
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let is_fork = input.get("fork").and_then(|v| v.as_bool()).unwrap_or(false)
            || subagent_type.as_deref() == Some("fork");

        Ok(Self {
            resume_thread_id,
            prompt,
            subagent_type,
            model,
            run_in_background,
            cwd,
            is_fork,
        })
    }
}

/// `resume_thread_id` 非法（非 UUID）时的明确拒绝：消息保留收到的值（有界），
/// 说明 task_id 不能用于 resume，并给出两条出路（用 child_thread_id / 省略字段）。
fn invalid_resume_thread_id(raw: &str) -> EffectiveToolError {
    EffectiveToolError::new(
        EffectiveToolErrorCode::InvalidInput,
        format!(
            "invalid resume_thread_id: {} — expected the child thread UUID reported as \
             `child_thread_id` (a background task id such as `bg-...` cannot be used to \
             resume); omit resume_thread_id to start a new sub-agent instead",
            peri_acp_types::session::bounded_error_message(raw, 120)
        ),
    )
}

impl super::SubAgentTool {
    pub(super) async fn validate_cwd(
        &self,
        explicit_cwd: Option<&str>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(cwd) = explicit_cwd {
            let requested = tokio::fs::canonicalize(cwd).await.map_err(|error| {
                EffectiveToolError::new(
                    EffectiveToolErrorCode::InvalidInput,
                    format!("cannot resolve explicit Agent.cwd: {error}"),
                )
            })?;
            let parent = tokio::fs::canonicalize(&self.parent_cwd)
                .await
                .map_err(|error| {
                    EffectiveToolError::new(
                        EffectiveToolErrorCode::InvalidInput,
                        format!("cannot resolve parent cwd for Agent.cwd validation: {error}"),
                    )
                })?;
            if requested != parent {
                return Err(EffectiveToolError::new(
                    EffectiveToolErrorCode::InvalidInput,
                    "Agent.cwd must resolve to the parent cwd; per-child Workspace rebinding is not supported",
                ).into());
            }
        }
        // 工具仍绑定父 Workspace；等价路径也统一使用父目录的表示。
        Ok(self.parent_cwd.clone())
    }

    /// Prefer the current ToolContext over the before_agent snapshot. Remove only
    /// the trailing AI tool call whose ToolResult has not arrived yet.
    pub(super) fn current_messages(&self, live_messages: &[BaseMessage]) -> Vec<BaseMessage> {
        let mut msgs: Vec<peri_agent::messages::BaseMessage> = if !live_messages.is_empty() {
            live_messages.to_vec()
        } else if let Some(ref pm) = self.parent_messages {
            pm.read().clone()
        } else {
            Vec::new()
        };
        if let Some(last) = msgs.last() {
            if last.has_tool_calls() {
                msgs.pop();
            }
        }
        msgs
    }
}
