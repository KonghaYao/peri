use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::{react::ToolCall, state::State},
    error::{AgentError, AgentResult},
    interaction::{
        ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
        UserInteractionBroker,
    },
    middleware::r#trait::Middleware,
};

use crate::tool_search::core_tools::{
    TOOL_AGENT, TOOL_BASH, TOOL_EDIT, TOOL_FOLDER_OPS, TOOL_WEBFETCH, TOOL_WEBSEARCH, TOOL_WRITE,
};

/// broker.request 超时（秒）：防止挂起 broker 导致 before_tool 永久阻塞
const BROKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub mod auto_classifier;
pub mod shared_mode;

pub use auto_classifier::{AutoClassifier, Classification, LlmAutoClassifier};
pub use peri_agent::hitl::{BatchItem, HitlDecision};
pub use shared_mode::{PermissionMode, SharedPermissionMode};

// ─── YOLO 模式检测 ─────────────────────────────────────────────────────────────

/// 检测是否处于 YOLO 模式（默认启用）
///
/// - `YOLO_MODE` 未设置或为 `true`/`1` → YOLO（跳过审批）
/// - `YOLO_MODE=false`/`0` → 启用 HITL 审批
pub fn is_yolo_mode() -> bool {
    std::env::var("YOLO_MODE")
        .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
        .unwrap_or(true)
}

// ─── 默认规则 ──────────────────────────────────────────────────────────────────

/// 默认敏感工具判断规则
///
/// - `bash`：所有 bash 命令
/// - `Write`：文件写入
/// - `Edit`：文件编辑
/// - `folder_operations`：目录操作
/// - `launch_agent`：子 Agent 委派（子 Agent 不含 HITL，可传递绕过审批）
/// - `cron_register`：定时任务注册（可定时触发任意 prompt，等价于代理执行权）
pub fn default_requires_approval(tool_name: &str) -> bool {
    tool_name == TOOL_BASH
        || tool_name == TOOL_FOLDER_OPS
        || tool_name == TOOL_AGENT
        || tool_name == TOOL_WRITE
        || tool_name == TOOL_EDIT
        || tool_name.starts_with("delete_")
        || tool_name.starts_with("rm_")
        || tool_name == TOOL_WEBFETCH
        || tool_name == TOOL_WEBSEARCH
        || tool_name.starts_with("mcp__")
        || tool_name == "cron_register"
}

/// 判断工具是否为文件编辑类工具（AcceptEdits 模式使用）
///
/// `Write`、`Edit`、`folder_operations` 归类为编辑工具，在 AcceptEdits 模式下自动放行。
/// `Bash`、`Agent`、`delete_*`、`rm_*` 不属于编辑工具，仍需审批。
pub fn is_edit_tool(tool_name: &str) -> bool {
    tool_name == TOOL_WRITE || tool_name == TOOL_EDIT || tool_name == TOOL_FOLDER_OPS
}

// ─── ExecuteExtraTool 权限透传 ─────────────────────────────────────────────

/// 获取有效的工具名称
///
/// 当 tool_name 为 [`crate::tool_search::core_tools::EXECUTE_EXTRA_TOOL_NAME`] 时，
/// 从 `input[EXTRA_TOOL_NAME_FIELD]` 提取目标工具名，用于 HITL 权限判断。
/// 否则直接返回原始工具名。
pub use crate::tool_search::core_tools::resolve_effective_tool_name as effective_tool_name;

// ─── HumanInTheLoopMiddleware ──────────────────────────────────────────────────

/// HumanInTheLoopMiddleware — 敏感工具调用前需用户确认
///
/// 在 `before_tool` 时拦截工具调用，通过注入的 [`UserInteractionBroker`] 请求用户审批。
///
/// # HITL 模式
/// 通过 `HumanInTheLoopMiddleware::new(...)` 或环境变量 `YOLO_MODE=false` 启用审批。
pub struct HumanInTheLoopMiddleware {
    broker: Option<Arc<dyn UserInteractionBroker>>,
    requires_approval: fn(&str) -> bool,
    /// 共享权限模式（动态切换），None 时走原有 Some/None broker 逻辑（向后兼容）
    mode: Option<Arc<SharedPermissionMode>>,
    /// Auto 模式的 LLM 分类器，仅在 mode=Auto 时使用
    auto_classifier: Option<Arc<dyn AutoClassifier>>,
    /// broker.request 超时，默认 300s；测试可设为短值
    broker_timeout: std::time::Duration,
}

impl HumanInTheLoopMiddleware {
    /// 创建启用的 HITL 中间件，使用注入的 broker
    pub fn new(
        broker: Arc<dyn UserInteractionBroker>,
        requires_approval: fn(&str) -> bool,
    ) -> Self {
        Self {
            broker: Some(broker),
            requires_approval,
            mode: None,
            auto_classifier: None,
            broker_timeout: BROKER_TIMEOUT,
        }
    }

    /// YOLO 模式：所有工具调用直接放行
    pub fn disabled() -> Self {
        Self {
            broker: None,
            requires_approval: default_requires_approval,
            mode: None,
            auto_classifier: None,
            broker_timeout: BROKER_TIMEOUT,
        }
    }

    /// 从环境变量决定是否启用（默认 YOLO；`YOLO_MODE=false` 则启用审批）
    pub fn from_env(
        broker: Arc<dyn UserInteractionBroker>,
        requires_approval: fn(&str) -> bool,
    ) -> Self {
        if is_yolo_mode() {
            Self::disabled()
        } else {
            Self::new(broker, requires_approval)
        }
    }

    /// 创建带共享权限模式的 HITL 中间件
    pub fn with_shared_mode(
        broker: Arc<dyn UserInteractionBroker>,
        requires_approval: fn(&str) -> bool,
        mode: Arc<SharedPermissionMode>,
        auto_classifier: Option<Arc<dyn AutoClassifier>>,
    ) -> Self {
        Self {
            broker: Some(broker),
            requires_approval,
            mode: Some(mode),
            auto_classifier,
            broker_timeout: BROKER_TIMEOUT,
        }
    }

    /// 设置 broker 审批超时（测试用）
    pub fn with_broker_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.broker_timeout = timeout;
        self
    }
}

/// 将 `ApprovalDecision` 映射为 `AgentResult<ToolCall>`
fn apply_decision(call: &ToolCall, decision: ApprovalDecision) -> AgentResult<ToolCall> {
    match decision {
        ApprovalDecision::Approve { .. } => Ok(call.clone()),
        ApprovalDecision::Edit { new_input } => {
            let mut modified = call.clone();
            modified.input = new_input;
            Ok(modified)
        }
        ApprovalDecision::Reject { reason, source: _ } => Err(AgentError::ToolRejected {
            tool: call.name.clone(),
            reason,
        }),
        ApprovalDecision::Respond { message } => Err(AgentError::ToolRejected {
            tool: call.name.clone(),
            reason: message,
        }),
    }
}

impl HumanInTheLoopMiddleware {
    /// 批量处理一批工具调用：收集所有需要审批的项，一次性弹窗，返回每个 call 的处理结果
    pub async fn process_batch(&self, calls: &[ToolCall]) -> Vec<AgentResult<ToolCall>> {
        let mut results: Vec<AgentResult<ToolCall>> = Vec::with_capacity(calls.len());

        // 快照当前 mode，确保整个批处理内评估一致（避免迭代过程中 mode 被外部修改）
        let mode_snapshot = self.mode.clone();

        for (i, call) in calls.iter().enumerate() {
            // 非敏感工具 → 直接放行（ExecuteExtraTool 透传目标工具名）
            let effective_name = effective_tool_name(&call.name, &call.input);
            if !(self.requires_approval)(&effective_name) {
                results.push(Ok(call.clone()));
                continue;
            }

            // 有 mode → 使用快照模式决策
            if let Some(mode) = &mode_snapshot {
                results.push(self.decide_by_mode(mode, call).await);
                continue;
            }

            // 无 mode 且无 broker → 放行
            let Some(broker) = &self.broker else {
                results.push(Ok(call.clone()));
                continue;
            };

            // 无 mode 但有 broker → 收集后批量弹窗
            return self
                .batch_broker_approve(broker, calls, i, &mut results)
                .await;
        }

        results
    }

    /// 通过 broker 请求用户审批单个工具调用
    async fn broker_approve(
        &self,
        broker: &Arc<dyn UserInteractionBroker>,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let ctx = InteractionContext::Approval {
            items: vec![ApprovalItem {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                tool_input: tool_call.input.clone(),
            }],
        };
        let response = match tokio::time::timeout(self.broker_timeout, broker.request(ctx)).await {
            Ok(resp) => resp,
            Err(_elapsed) => {
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason: format!("审批超时 ({} 秒)", BROKER_TIMEOUT.as_secs()),
                });
            }
        };
        let decision = match response {
            InteractionResponse::Decisions(mut d) => d.pop().unwrap_or(ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            }),
            _ => ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            },
        };
        apply_decision(tool_call, decision)
    }

    /// 根据共享权限模式决策单个工具调用
    async fn decide_by_mode(
        &self,
        mode: &Arc<SharedPermissionMode>,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        match mode.load() {
            PermissionMode::Bypass => Ok(tool_call.clone()),
            PermissionMode::DontAsk => Err(AgentError::ToolRejected {
                tool: tool_call.name.clone(),
                reason: "Don't Ask 模式：自动拒绝".to_string(),
            }),
            PermissionMode::AcceptEdit => {
                if is_edit_tool(&tool_call.name) {
                    Ok(tool_call.clone())
                } else {
                    match &self.broker {
                        Some(broker) => self.broker_approve(broker, tool_call).await,
                        None => Ok(tool_call.clone()),
                    }
                }
            }
            PermissionMode::AutoMode => match &self.auto_classifier {
                Some(classifier) => {
                    let result = classifier.classify(&tool_call.name, &tool_call.input).await;
                    match result {
                        Classification::Allow => Ok(tool_call.clone()),
                        Classification::Deny => Err(AgentError::ToolRejected {
                            tool: tool_call.name.clone(),
                            reason: "Auto 模式：分类器拒绝".to_string(),
                        }),
                        Classification::Unsure => match &self.broker {
                            Some(broker) => self.broker_approve(broker, tool_call).await,
                            None => Err(AgentError::ToolRejected {
                                tool: tool_call.name.clone(),
                                reason: "Auto 模式：分类器不确定且无 broker".to_string(),
                            }),
                        },
                    }
                }
                None => match &self.broker {
                    Some(broker) => self.broker_approve(broker, tool_call).await,
                    None => Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason: "Auto 模式：无分类器且无 broker".to_string(),
                    }),
                },
            },
            PermissionMode::Default => match &self.broker {
                Some(broker) => self.broker_approve(broker, tool_call).await,
                None => {
                    tracing::warn!("HITL Default 模式但无 broker，拒绝工具调用");
                    Err(anyhow::anyhow!("HITL 审批不可用：未配置 broker").into())
                }
            },
        }
    }

    /// 无 mode 时原有的批量 broker 审批逻辑（向后兼容）
    async fn batch_broker_approve(
        &self,
        broker: &Arc<dyn UserInteractionBroker>,
        calls: &[ToolCall],
        start_idx: usize,
        initial_results: &mut Vec<AgentResult<ToolCall>>,
    ) -> Vec<AgentResult<ToolCall>> {
        let mut results: Vec<AgentResult<ToolCall>> = std::mem::take(initial_results);

        let needs_approval: Vec<(usize, &ToolCall)> = calls
            .iter()
            .enumerate()
            .skip(start_idx)
            .filter(|(_, c)| (self.requires_approval)(&effective_tool_name(&c.name, &c.input)))
            .collect();

        if needs_approval.is_empty() {
            results.extend(calls.iter().skip(start_idx).map(|c| Ok(c.clone())));
            return results;
        }

        let items: Vec<ApprovalItem> = needs_approval
            .iter()
            .map(|(_, c)| ApprovalItem {
                tool_call_id: c.id.clone(),
                tool_name: c.name.clone(),
                tool_input: c.input.clone(),
            })
            .collect();

        let ctx = InteractionContext::Approval { items };
        let response = match tokio::time::timeout(self.broker_timeout, broker.request(ctx)).await {
            Ok(resp) => resp,
            Err(_elapsed) => {
                results.push(Err(AgentError::ToolRejected {
                    tool: "batch_approval".to_string(),
                    reason: format!("审批超时 ({} 秒)", BROKER_TIMEOUT.as_secs()),
                }));
                results.extend(calls.iter().skip(start_idx).map(|c| {
                    Err(AgentError::ToolRejected {
                        tool: c.name.clone(),
                        reason: "审批超时".to_string(),
                    })
                }));
                return results;
            }
        };

        let decisions = match response {
            InteractionResponse::Decisions(d) => d,
            _ => vec![
                ApprovalDecision::Reject {
                    reason: "unexpected response".to_string(),
                    source: None,
                };
                needs_approval.len()
            ],
        };

        let mut decision_iter = decisions.into_iter();

        for call in calls.iter().skip(start_idx) {
            if (self.requires_approval)(&effective_tool_name(&call.name, &call.input)) {
                let decision = decision_iter.next().unwrap_or(ApprovalDecision::Reject {
                    reason: "用户拒绝".to_string(),
                    source: None,
                });
                results.push(apply_decision(call, decision));
            } else {
                results.push(Ok(call.clone()));
            }
        }

        results
    }
}

#[async_trait]
impl<S: State> Middleware<S> for HumanInTheLoopMiddleware {
    fn name(&self) -> &str {
        "HumanInTheLoopMiddleware"
    }

    /// 批量工具调用前处理：对一批工具调用一次性收集所有需审批的项，
    /// 通过 broker 弹出一个 [多工具审批] 弹窗，避免逐个弹窗打断用户。
    async fn before_tools_batch(
        &self,
        _state: &mut S,
        calls: &[ToolCall],
    ) -> Vec<AgentResult<ToolCall>> {
        self.process_batch(calls).await
    }

    async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
        // 1. 非敏感工具 → 所有模式都放行
        if !(self.requires_approval)(&effective_tool_name(&tool_call.name, &tool_call.input)) {
            return Ok(tool_call.clone());
        }

        // 2. 有 mode → 按权限模式决策
        if let Some(mode) = &self.mode {
            return self.decide_by_mode(mode, tool_call).await;
        }

        // 3. 无 mode 且无 broker → 放行（disabled() 路径）
        let Some(broker) = &self.broker else {
            return Ok(tool_call.clone());
        };

        // 4. 无 mode 但有 broker → 原有弹窗审批逻辑
        self.broker_approve(broker, tool_call).await
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
