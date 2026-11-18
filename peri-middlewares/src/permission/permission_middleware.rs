use super::async_trait;
use super::hook_state;
use super::AgentError;
use super::AgentResult;
use super::ApprovalDecision;
use super::ApprovalItem;
use super::Arc;
use super::AutoClassifier;
use super::Classification;
use super::InteractionContext;
use super::InteractionResponse;
use super::Middleware;
use super::PermissionMode;
use super::PromptSection;
use super::SharedPermissionMode;
use super::ToolCall;
use super::ToolRejectionCause;
use super::TrustedInteractionOwner;
use super::UnansweredCause;
use super::UserInteractionBroker;
use super::TOOL_AGENT;
use super::TOOL_BASH;
use super::TOOL_EDIT;
use super::TOOL_FOLDER_OPS;
use super::TOOL_WEBFETCH;
use super::TOOL_WEBSEARCH;
use super::TOOL_WRITE;
use std::fmt;
use std::fmt::{Debug, Formatter};

/// broker.request 超时（秒）：防止挂起 broker 导致 before_tool 永久阻塞
pub(crate) const BROKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

// ─── 默认规则 ──────────────────────────────────────────────────────────────────

pub(super) const CRON_REGISTER_TOOL_NAME: &str = "cron_register";
pub(super) const MONITOR_START_TOOL_NAME: &str = "monitor_start";
pub(super) const WORKFLOW_TOOL_NAME: &str = "Workflow";

/// 默认敏感工具判断规则
///
/// builtin direct 工具使用原名，deferred 工具仍可能使用前缀名。命中声明表
/// （[`peri_acp_types::builtin_mcp::original_tool_name_of_effective`]，IF-D15 唯一归一入口）则改用原始名走下面的
/// 分支；未命中前缀名沿用保守语义。raw 外部 MCP 来源由绑定目标强制审批。
/// 名字字面量只在 `peri_acp_types::builtin_mcp` 声明一份，本模块不得复制。
///
/// - `bash`：所有 bash 命令
/// - `Write`：文件写入
/// - `Edit`：文件编辑
/// - `folder_operations`：目录操作
/// - `Agent`：子 Agent 委派（子 Agent 不含 HITL，可传递绕过审批）
/// - `Workflow`：工作流启动（后台子链不含 HITL，等价于委派执行权）
/// - `cron_register`：定时任务注册（可定时触发任意 prompt，等价于代理执行权）
/// - `monitor_start`：启动监控命令（可执行任意 shell，等价于命令执行权）
#[must_use]
pub fn default_requires_approval(tool_name: &str) -> bool {
    match super::original_tool_name_of_effective(tool_name) {
        Some(original_name) => default_requires_approval_without_builtin(original_name),
        None => default_requires_approval_without_builtin(tool_name),
    }
}

/// [`default_requires_approval`] 的分支本体（自迁移前实现逐字位移，含 `mcp__` 前缀行）。
pub(super) fn default_requires_approval_without_builtin(tool_name: &str) -> bool {
    tool_name == TOOL_BASH
        || tool_name == TOOL_FOLDER_OPS
        || tool_name == TOOL_AGENT
        || tool_name == WORKFLOW_TOOL_NAME
        || tool_name == TOOL_WRITE
        || tool_name == TOOL_EDIT
        || tool_name.starts_with("delete_")
        || tool_name.starts_with("rm_")
        || tool_name == TOOL_WEBFETCH
        || tool_name == TOOL_WEBSEARCH
        || tool_name.starts_with("mcp__")
        || tool_name == CRON_REGISTER_TOOL_NAME
        || tool_name == MONITOR_START_TOOL_NAME
}

/// 判断工具是否为文件编辑类工具（AcceptEdits 模式使用）
///
/// `Write`、`Edit`、`folder_operations` 归类为编辑工具，在 AcceptEdits 模式下自动放行。
/// `Bash`、`Agent`、`delete_*`、`rm_*` 不属于编辑工具，仍需审批。
/// builtin 一等工具按原始名判定；raw 外部 MCP 不作为编辑工具自动放行。
#[must_use]
pub fn is_edit_tool(tool_name: &str) -> bool {
    match super::original_tool_name_of_effective(tool_name) {
        Some(original_name) => is_edit_tool_without_builtin(original_name),
        None => is_edit_tool_without_builtin(tool_name),
    }
}

/// [`is_edit_tool`] 的分支本体（自迁移前实现逐字位移）。
pub(super) fn is_edit_tool_without_builtin(tool_name: &str) -> bool {
    tool_name == TOOL_WRITE || tool_name == TOOL_EDIT || tool_name == TOOL_FOLDER_OPS
}

pub(crate) fn is_external_mcp(origin: Option<&hook_state::BoundToolOrigin>) -> bool {
    matches!(origin, Some(origin) if origin.mcp_server_name.is_some() && origin.builtin_mcp_instance.is_none())
}

/// Auto 分类与缓存必须区分外部 MCP 的真实目标，执行调用名仍保持原样。
pub(super) fn classifier_tool_name(
    call: &ToolCall,
    origin: Option<&hook_state::BoundToolOrigin>,
) -> String {
    if !is_external_mcp(origin) {
        return call.name.clone();
    }
    let origin = origin.expect("external MCP 必须有绑定来源");
    serde_json::json!({
        "kind": "external_mcp",
        "server": origin.mcp_server_name.as_deref().unwrap_or_default(),
        "wire_tool": origin.mcp_tool_name.as_deref().unwrap_or(&call.name),
    })
    .to_string()
}

// ─── PermissionMiddleware ──────────────────────────────────────────────────

/// PermissionMiddleware — 敏感工具调用前需用户确认
///
/// 在 `before_tool` 时拦截工具调用，通过注入的 [`UserInteractionBroker`] 请求用户审批。
///
/// # HITL 模式
/// 通过 `PermissionMiddleware::new(...)` 或权限模式启用审批。
pub struct PermissionMiddleware {
    pub(super) broker: Option<Arc<dyn UserInteractionBroker>>,
    pub(super) requires_approval: fn(&str) -> bool,
    /// 共享权限模式（动态切换），None 时走原有 Some/None broker 逻辑（向后兼容）
    pub(super) mode: Option<Arc<SharedPermissionMode>>,
    /// Auto 模式的 LLM 分类器，仅在 mode=Auto 时使用
    pub(super) auto_classifier: Option<Arc<dyn AutoClassifier>>,
    /// broker.request 超时，默认 300s；测试可设为短值
    pub(super) broker_timeout: std::time::Duration,
}

impl Debug for PermissionMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PermissionMiddleware")
            .finish_non_exhaustive()
    }
}

impl PermissionMiddleware {
    pub(super) fn requires_approval_for(
        &self,
        call: &ToolCall,
        origin: Option<&hook_state::BoundToolOrigin>,
    ) -> bool {
        // A raw external MCP name can equal a trusted builtin name such as Read.
        // The pinned dispatch target, rather than the spelling, decides its policy.
        // Malformed proxy calls fail closed: normalization cannot recover a target,
        // so they must not inherit a permissive default.
        let malformed_proxy = peri_acp_types::tool_proxy::is_invoke_proxy_name(&call.name)
            && crate::tool_search::core_tools::parse_proxy_call(&call.input).is_err();
        is_external_mcp(origin)
            || malformed_proxy
            || (self.requires_approval)(&super::effective_tool_name(&call.name, &call.input))
    }

    /// Frozen approval guidance. Tool availability comes from the live request,
    /// and sensitivity is evaluated against the bound target when a call runs.
    #[must_use]
    pub fn sections() -> Vec<PromptSection> {
        vec![PromptSection::declared(
            peri_acp_types::meta_harness::sections::APPROVAL,
            peri_agent::middleware::PromptSectionContent::Builtin(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/prompts/sections/10_hitl.md"
            ))),
        )]
    }

    /// 创建启用的 HITL 中间件，使用注入的 broker
    #[must_use]
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

    /// 禁用模式：所有工具调用直接放行（无审批）
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            broker: None,
            requires_approval: default_requires_approval,
            mode: None,
            auto_classifier: None,
            broker_timeout: BROKER_TIMEOUT,
        }
    }

    /// 审批通道是否有效（H2 有效模式判定；实例级）。
    ///
    /// `disabled()`（无 broker、无 mode）为 false：工具调用直接放行，声明
    /// 10_hitl 会让模型等待永不到来的审批，因此不声明该段——「存在不等于需要
    /// 审批说明」，装配事实之外还要看持有者的有效模式。
    ///
    /// 规则唯一事实源是 [`Self::approval_active_from`]：装配面（`for_workflow` /
    /// 主链 `with_shared_mode`）与段落投影（`prompt_policy`）都消费同一函数，
    /// 不允许出现两份漂移的判定。
    #[must_use]
    pub fn approval_active(&self) -> bool {
        Self::approval_active_from(self.broker.is_some(), self.mode.is_some())
    }

    /// 有效审批判定（构造输入级）：broker 或共享 mode **任一存在** ⇒ 审批通道
    /// 有效；两者皆无 ⇒ 放行（disabled）。
    #[must_use]
    pub fn approval_active_from(broker_present: bool, mode_present: bool) -> bool {
        broker_present || mode_present
    }

    /// workflow 链的审批装配与判定（**同一规则的两个出口**）：
    ///
    /// `broker` 与共享 `permission_mode` **齐备**才构造启用共享模式的实例
    /// （自主后台 agent 的既有语义：缺任一即 Bypass）；否则 `disabled()`
    /// （无审批通道，段落投影随之为 10_hitl 缺席）。
    #[must_use]
    pub fn for_workflow(
        broker: Option<Arc<dyn UserInteractionBroker>>,
        mode: Option<Arc<SharedPermissionMode>>,
    ) -> Self {
        match (broker, mode) {
            (Some(broker), Some(mode)) => {
                Self::with_shared_mode(broker, default_requires_approval, mode, None)
            }
            _ => Self::disabled(),
        }
    }

    /// workflow 链的有效审批事实（与 [`Self::for_workflow`] 同一规则；
    /// 段落投影在无链构造点消费它）。
    #[must_use]
    pub fn workflow_approval_active(broker_present: bool, mode_present: bool) -> bool {
        matches!((broker_present, mode_present), (true, true))
    }

    /// 创建带共享权限模式的 HITL 中间件
    #[must_use]
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
    #[must_use]
    pub fn with_broker_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.broker_timeout = timeout;
        self
    }
}

/// 将 `ApprovalDecision` 映射为 `AgentResult<ToolCall>`。
///
/// **只用于「用户 / 客户端已作出决定」的路径**：`Reject` / `Respond` 一律记为
/// [`ToolRejectionCause::UserRejected`]。没有人的决定的拒绝（无人可答、本端策略）
/// 必须走各自的显式构造（[`unanswered_rejection`] / `PolicyDenied`），不得借本函数
/// 伪造一个 `ApprovalDecision` 再压成「用户拒绝」（裁决 R9）。
pub(crate) fn apply_decision(call: &ToolCall, decision: ApprovalDecision) -> AgentResult<ToolCall> {
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
            cause: ToolRejectionCause::UserRejected,
        }),
        ApprovalDecision::Respond { message } => Err(AgentError::ToolRejected {
            tool: call.name.clone(),
            reason: message,
            cause: ToolRejectionCause::UserRejected,
        }),
    }
}

/// 「无人可答」的显式拒绝：原因**逐条**来自交互契约，typed cause 随错误走
/// （裁决 R9：不再写带外记录点，也不伪造 `ApprovalDecision::Reject`）。
pub(super) fn unanswered_rejection(call: &ToolCall, cause: UnansweredCause) -> AgentError {
    AgentError::ToolRejected {
        tool: call.name.clone(),
        reason: cause.reason_text().to_string(),
        cause: ToolRejectionCause::ApprovalUnavailable(cause),
    }
}

pub(super) fn classifier_context(
    state: &dyn hook_state::StateView,
    call: &ToolCall,
) -> Option<peri_model::ModelCallContext> {
    Some(peri_model::ModelCallContext {
        owner: state.session_id()?.to_owned(),
        execution: state.execution_binding()?.attempt_id.to_string(),
        call: format!("auto-classify:{}:{}", state.current_step()?, call.id),
        purpose: "auto-classify".into(),
        parent: None,
    })
}

impl PermissionMiddleware {
    /// 批量处理一批工具调用：收集所有需要审批的项，一次性弹窗，返回每个 call 的处理结果。
    ///
    /// `owner` 是本次审批的可信归属（同一批次 = 同一 turn owner）；`None`
    /// （无调用期状态）时，需要弹窗的条目按
    /// [`crate::interaction_owner::UNTRUSTED_OWNER_REASON`] 显式拒绝，不静默放行。
    pub(super) async fn process_batch_with_origins(
        &self,
        calls: &[ToolCall],
        origins: &[Option<hook_state::BoundToolOrigin>],
        owner: Option<&TrustedInteractionOwner>,
        state: Option<&dyn hook_state::StateView>,
    ) -> Vec<AgentResult<ToolCall>> {
        let mut results: Vec<AgentResult<ToolCall>> = Vec::with_capacity(calls.len());

        // 快照当前 mode，确保整个批处理内评估一致（避免迭代过程中 mode 被外部修改）
        let mode_snapshot = self.mode.clone();

        {
            let indexed_calls = calls.iter().enumerate();
            for (i, call) in indexed_calls {
                // Canonical dispatch has already projected method-based tools. Keep
                // the wrapper fallback for legacy/static call paths that still pass
                // ExecuteExtraTool directly.
                if !self.requires_approval_for(call, origins[i].as_ref()) {
                    results.push(Ok(call.clone()));
                    continue;
                }

                // 有 mode → 使用快照模式决策
                if let Some(mode) = &mode_snapshot {
                    results.push(
                        self.decide_by_mode(
                            mode,
                            call,
                            origins[i].as_ref(),
                            owner,
                            state.and_then(|state| classifier_context(state, call)),
                        )
                        .await,
                    );
                    continue;
                }

                // 无 mode 且无 broker → 放行
                let Some(broker) = &self.broker else {
                    results.push(Ok(call.clone()));
                    continue;
                };

                // 无 mode 但有 broker → 收集后批量弹窗
                return self
                    .batch_broker_approve(broker, calls, origins, owner, i, &mut results)
                    .await;
            }
        }

        results
    }

    /// 通过 broker 请求用户审批单个工具调用
    ///
    /// `owner` 为本次审批的可信归属；缺失时显式拒绝（fail-closed），不伪造归属。
    pub(super) async fn broker_approve(
        &self,
        owner: Option<&TrustedInteractionOwner>,
        broker: &Arc<dyn UserInteractionBroker>,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let Some(owner) = owner.cloned() else {
            return Err(crate::interaction_owner::untrusted_owner_error(
                &tool_call.name,
            ));
        };
        let ctx = InteractionContext::Approval {
            items: vec![ApprovalItem {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                tool_input: tool_call.input.clone(),
            }],
        };
        let response =
            match peri_time::timeout(self.broker_timeout, broker.request(owner, ctx)).await {
                Ok(resp) => resp,
                Err(_elapsed) => {
                    // 审批未在期限内给出决定：没有任何人的决定，也不是「无人可答」
                    //（本端审批通道存在，只是没回答）⇒ 本端机制拒绝（裁决 R9）。
                    return Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason: format!("审批超时 ({} 秒)", BROKER_TIMEOUT.as_secs()),
                        cause: ToolRejectionCause::PolicyDenied,
                    });
                }
            };
        let decision = match response {
            InteractionResponse::Decisions(mut d) => d.pop().unwrap_or(ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            }),
            // 无人可作答（含「本部署没有在线 endpoint」）：按未获批准处理，并如实
            // 转述原因，绝不伪造「用户拒绝」。typed cause 随错误本身走
            //（裁决 R9：消费方无需任何带外记录点即可区分两类事实）。
            InteractionResponse::Unanswered { cause } => {
                return Err(unanswered_rejection(tool_call, cause));
            }
            _ => ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            },
        };
        apply_decision(tool_call, decision)
    }

    /// 根据共享权限模式决策单个工具调用
    pub(super) async fn decide_by_mode(
        &self,
        mode: &Arc<SharedPermissionMode>,
        tool_call: &ToolCall,
        origin: Option<&hook_state::BoundToolOrigin>,
        owner: Option<&TrustedInteractionOwner>,
        model_context: Option<peri_model::ModelCallContext>,
    ) -> AgentResult<ToolCall> {
        match mode.load() {
            PermissionMode::Bypass => Ok(tool_call.clone()),
            PermissionMode::AcceptEdit => {
                if !is_external_mcp(origin) && is_edit_tool(&tool_call.name) {
                    Ok(tool_call.clone())
                } else {
                    match &self.broker {
                        Some(broker) => self.broker_approve(owner, broker, tool_call).await,
                        None => Ok(tool_call.clone()),
                    }
                }
            }
            PermissionMode::AutoMode => match &self.auto_classifier {
                Some(classifier) => {
                    let classification_name = classifier_tool_name(tool_call, origin);
                    let result = classifier
                        .classify_with_context(
                            &classification_name,
                            &tool_call.input,
                            model_context,
                        )
                        .await;
                    match result {
                        Classification::Allow => Ok(tool_call.clone()),
                        // 分类器是本端自动策略：拒绝来自策略，不是任何人的决定。
                        Classification::Deny => Err(AgentError::ToolRejected {
                            tool: tool_call.name.clone(),
                            reason: "Auto 模式：分类器拒绝".to_string(),
                            cause: ToolRejectionCause::PolicyDenied,
                        }),
                        Classification::Unsure => match &self.broker {
                            Some(broker) => self.broker_approve(owner, broker, tool_call).await,
                            // 没有分类器 → 没有可作答的审批通道 ⇒ 本端机制拒绝。
                            None => Err(AgentError::ToolRejected {
                                tool: tool_call.name.clone(),
                                reason: "Auto 模式：分类器不确定且无 broker".to_string(),
                                cause: ToolRejectionCause::PolicyDenied,
                            }),
                        },
                    }
                }
                None => match &self.broker {
                    Some(broker) => self.broker_approve(owner, broker, tool_call).await,
                    None => Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason: "Auto 模式：无分类器且无 broker".to_string(),
                        cause: ToolRejectionCause::PolicyDenied,
                    }),
                },
            },
            PermissionMode::Default => match &self.broker {
                Some(broker) => self.broker_approve(owner, broker, tool_call).await,
                None => {
                    tracing::warn!("HITL Default 模式但无 broker，拒绝工具调用");
                    Err(anyhow::anyhow!("HITL 审批不可用：未配置 broker").into())
                }
            },
        }
    }

    /// 无 mode 时原有的批量 broker 审批逻辑（向后兼容）
    // 审批注入面：与单条路径（`broker_approve`）同形的 broker / owner，
    // 加上批次切片（calls / origins / start_idx）与结果累积，逐项传递。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn batch_broker_approve(
        &self,
        broker: &Arc<dyn UserInteractionBroker>,
        calls: &[ToolCall],
        origins: &[Option<hook_state::BoundToolOrigin>],
        owner: Option<&TrustedInteractionOwner>,
        start_idx: usize,
        initial_results: &mut Vec<AgentResult<ToolCall>>,
    ) -> Vec<AgentResult<ToolCall>> {
        let mut results: Vec<AgentResult<ToolCall>> = std::mem::take(initial_results);

        let needs_approval: Vec<(usize, &ToolCall)> = calls
            .iter()
            .enumerate()
            .skip(start_idx)
            .filter(|(i, c)| self.requires_approval_for(c, origins[*i].as_ref()))
            .collect();

        if needs_approval.is_empty() {
            results.extend(calls.iter().skip(start_idx).map(|c| Ok(c.clone())));
            return results;
        }

        // 无可信归属 ⇒ 本批**待审批**条目显式拒绝（结果长度与输入一致）。
        //
        // 粒度与正常路径（本函数末尾的逐条循环）一致：不需要审批的调用照常放行，
        // 不因同批另一条缺少归属而被连带拒绝。
        let Some(owner) = owner.cloned() else {
            {
                let indexed_calls = calls.iter().enumerate().skip(start_idx);
                for (i, call) in indexed_calls {
                    if self.requires_approval_for(call, origins[i].as_ref()) {
                        results.push(Err(crate::interaction_owner::untrusted_owner_error(
                            &call.name,
                        )));
                    } else {
                        results.push(Ok(call.clone()));
                    }
                }
            }
            return results;
        };

        let items: Vec<ApprovalItem> = needs_approval
            .iter()
            .map(|(_, c)| ApprovalItem {
                tool_call_id: c.id.clone(),
                tool_name: c.name.clone(),
                tool_input: c.input.clone(),
            })
            .collect();

        let ctx = InteractionContext::Approval { items };
        let response =
            match peri_time::timeout(self.broker_timeout, broker.request(owner, ctx)).await {
                Ok(resp) => resp,
                Err(_elapsed) => {
                    // 批量结果必须与输入等长：`MiddlewareChain::run_before_tools_batch`
                    // 会对长度不符 fail closed，因此超时按每条调用各返一条拒绝，
                    // 不额外插入汇总条目。
                    results.extend(calls.iter().skip(start_idx).map(|c| {
                        Err(AgentError::ToolRejected {
                            tool: c.name.clone(),
                            reason: format!("审批超时 ({} 秒)", BROKER_TIMEOUT.as_secs()),
                            // 审批未在期限内给出决定：没有任何人的决定（裁决 R9）。
                            cause: ToolRejectionCause::PolicyDenied,
                        })
                    }));
                    return results;
                }
            };

        // 无人可作答（含「本部署没有任何在线 endpoint」）：逐条如实转述原因并带上
        // typed cause（裁决 R9 之前这里写带外 sink，现已不需要）。
        //
        // 逐条重建**按调用顺序**的结果（与下方正常路径同形）：结果必须与输入逐位
        // 对应，免审调用照常放行。
        let decisions = match response {
            InteractionResponse::Decisions(d) => d,
            InteractionResponse::Unanswered { cause } => {
                {
                    let indexed_calls = calls.iter().enumerate().skip(start_idx);
                    for (i, call) in indexed_calls {
                        if self.requires_approval_for(call, origins[i].as_ref()) {
                            results.push(Err(unanswered_rejection(call, cause)));
                        } else {
                            results.push(Ok(call.clone()));
                        }
                    }
                }
                return results;
            }
            _ => vec![
                ApprovalDecision::Reject {
                    reason: "unexpected response".to_string(),
                    source: None,
                };
                needs_approval.len()
            ],
        };

        let mut decision_iter = decisions.into_iter();

        {
            let indexed_calls = calls.iter().enumerate().skip(start_idx);
            for (i, call) in indexed_calls {
                if self.requires_approval_for(call, origins[i].as_ref()) {
                    let decision = decision_iter.next().unwrap_or(ApprovalDecision::Reject {
                        reason: "用户拒绝".to_string(),
                        source: None,
                    });
                    results.push(apply_decision(call, decision));
                } else {
                    results.push(Ok(call.clone()));
                }
            }
        }

        results
    }
}

#[async_trait]
impl Middleware for PermissionMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::Permission.name()
    }

    /// 声明持有的系统提示词段落（10_hitl，内容载体；装配期收集，契约 2）。
    ///
    /// 有效模式判定（H2）：审批通道无效（`disabled()`——无 broker、无 mode）
    /// 时不声明 10_hitl，与 `prompt_policy::collect_prompt_sections` 的能力
    /// 投影同源；workflow 链的 disabled 实例因此不会向模型声明审批机制。
    fn prompt_sections(&self) -> Vec<PromptSection> {
        if !self.approval_active() {
            return Vec::new();
        }
        Self::sections()
    }

    /// 批量工具调用前处理：对一批工具调用一次性收集所有需审批的项，
    /// 通过 broker 弹出一个 `[多工具审批]` 弹窗，避免逐个弹窗打断用户。
    async fn before_tools_batch(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        calls: &[ToolCall],
    ) -> Vec<AgentResult<ToolCall>> {
        let origins: Vec<_> = calls
            .iter()
            .map(|call| state.tool_origin(&call.id))
            .collect();
        // 归属在弹窗前一次性铸造（同一批次 = 同一 turn owner）；缺失即拒绝。
        let owner = crate::interaction_owner::owner_from_hook_state(state);
        self.process_batch_with_origins(calls, &origins, owner.as_ref(), Some(state))
            .await
    }

    async fn before_tool(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        // 1. 非敏感工具 → 所有模式都放行
        let origin = state.tool_origin(&tool_call.id);
        if !self.requires_approval_for(tool_call, origin.as_ref()) {
            return Ok(tool_call.clone());
        }

        // 2. 有 mode → 按权限模式决策
        if let Some(mode) = &self.mode {
            // 归属只在真正需要弹窗的分支被消费：Bypass / 免审批分支不铸造、
            // 不受缺失归属影响（显式 Bypass 仍由原 Permission 权威裁决）。
            let owner = crate::interaction_owner::owner_from_hook_state(state);
            return self
                .decide_by_mode(
                    mode,
                    tool_call,
                    origin.as_ref(),
                    owner.as_ref(),
                    classifier_context(state, tool_call),
                )
                .await;
        }

        // 3. 无 mode 且无 broker → 放行（disabled() 路径）
        let Some(broker) = &self.broker else {
            return Ok(tool_call.clone());
        };

        // 4. 无 mode 但有 broker → 原有弹窗审批逻辑
        let owner = crate::interaction_owner::owner_from_hook_state(state);
        self.broker_approve(owner.as_ref(), broker, tool_call).await
    }
}
