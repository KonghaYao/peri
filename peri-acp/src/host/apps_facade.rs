//! `SessionAppsFacade`：七个 `peri/mcp/apps/*` 方法的唯一实现（设计 §2.1、§4.4、§8）。
//!
//! 权威归属（本层只做**组合**，不铸造任何身份）：
//! - **附件 / namespace / 纪元**：`SessionInteractionRouter` 分配，本层把返回值
//!   **原样**写进 `AttachmentBinding`（自铸纪元会让 record 与 router 的纪元不一致，
//!   审批路由随即不匹配）；
//! - **序号推进与去重**：[`RequestLedger`]（同 key 只登记一次）；
//! - **执行**：`SessionInvocationPort`（session-owned canonical 入口，与 App 回调
//!   同一管线）；本层不自行调用 MCP peer；
//! - **记录事实**：`McpAppsRelayPort` 的 App facts 面（descriptors / attach 快照 /
//!   资源读取 / rerun 目标）。
//!
//! 错误分工（裁决 D3，收敛成一处）：
//! - **请求本身不合法**（方法不存在、缺 / 错 `sessionId`、会话不存在、连接不是该
//!   会话的在线 endpoint、连接不是该附件的承载连接、非本连接的 Host 域）⇒
//!   [`AcpError`]，kind 稳定、可按键区分；
//! - **业务判决**（§8 六种情况，含 `UnknownNamespace`）⇒ in-band
//!   `RequestVerdict`（同一个 error 不得承载多种事实）；
//! - **执行事实** ⇒ `AppsRequestOutcome`。
//!
//! fail-closed：缺归属 / 无在线 endpoint / 未附着 / 未批准一律**显式拒绝或显式
//! 未执行**；不伪造 turn、不自动批准、不静默降级。「未获批准」与「用户拒绝」由
//! service 的 typed outcome 区分，本层不改写它的结论。
//!
//! entitlement（`SessionAuthorizationPort`）在 4a **不被消费**：本层与能力声明都
//! **不得**声称已强制（`EntitlementState::Unenforced` 保持，§8.2）。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppsErrorKind, McpAppsRelayPort, NotExecutedCause,
    SessionInvocationOutcome, SessionInvocationPort, SessionInvocationRequest,
};
use peri_acp_types::mcp_apps_facade::{
    AppAttachRequest, AppAttachSnapshot, AppsAttachRequest, AppsAttachResponse, AppsCancelRequest,
    AppsCancelResponse, AppsCloseRequest, AppsCloseResponse, AppsDetachRequest, AppsDetachResponse,
    AppsListRequest, AppsListResponse, AppsOperation, AppsRequestOutcome, AppsRequestRequest,
    AppsRequestResponse, AppsRerunRequest, AttachmentBinding, AttachmentDetachReason,
    DetachVerdict, HostNamespace, McpAppsFeatures, McpAppsLimits, RequestNamespaceId, RequestSeq,
    RequestVerdict, ResourceListRequest, ResourceReadRequest, REQUEST_WAIT_TIMEOUT_MS,
};
use peri_acp_types::tool_invocation::InvocationOrigin;
use serde_json::Value;

use super::apps_ledger::{
    not_executed_before_commit, AdmissionVerdict, LedgerBudget, NamespaceKind, NamespaceOpenError,
    PendingRequest, RequestCancelReason, RequestFingerprint, RequestLedger, RequestOperation,
};
use super::interaction_router::{
    AttachRejection, DetachVerdict as RouterDetachVerdict, SessionAttachment,
    SessionInteractionRouter, ViewAttachRequest,
};
use crate::transport::types::AcpError;

/// MCP Apps 请求的 ACP 错误构造（kind 稳定：客户端按键区分，不得一 kind 多事实）。
pub(crate) fn apps_error(kind: McpAppsErrorKind) -> AcpError {
    let kind =
        serde_json::to_value(kind).unwrap_or(Value::String("upstream_protocol_error".into()));
    AcpError::new(-32000, "MCP Apps request failed").with_data(serde_json::json!({"kind": kind}))
}

/// in-band 判决 → 错误面投影（只用于结果形状无法承载判决的响应，例如 `apps/attach`
/// 与旧别名的同步 wire）。
pub(crate) fn verdict_error(verdict: RequestVerdict) -> AcpError {
    apps_error(
        verdict
            .error_kind()
            .unwrap_or(McpAppsErrorKind::UpstreamProtocolError),
    )
}

/// 旧别名的 record 事实投影（descriptor + 绑定的 MCP 协议版本）。
pub(crate) struct AliasRecordFacts {
    pub(crate) descriptor: peri_acp_types::mcp_apps_facade::AppDescriptor,
    pub(crate) mcp_protocol_version: String,
}

/// 一次已准入请求的执行计划（与 fingerprint 的输入同源构造，避免第二份目标推导）。
enum OperationPlan {
    AppToolsCall {
        record_id: String,
        server_generation: u64,
        attachment_id: AttachmentId,
        epoch: ConnectionEpoch,
        tool_name: String,
        arguments: serde_json::Map<String, Value>,
    },
    HostToolsCall {
        server_id: String,
        tool_name: String,
        arguments: serde_json::Map<String, Value>,
    },
    ResourcesRead {
        app_id: String,
        attachment_id: AttachmentId,
        epoch: ConnectionEpoch,
        uri: String,
    },
    ResourcesList {
        app_id: String,
        attachment_id: AttachmentId,
        epoch: ConnectionEpoch,
        cursor: Option<String>,
    },
}

/// session 级 MCP Apps facade（随会话环境存活；进程重启不恢复任何执行状态）。
pub(crate) struct SessionAppsFacade {
    session_id: String,
    runtime_epoch: u64,
    router: Arc<SessionInteractionRouter>,
    relay: Arc<dyn McpAppsRelayPort>,
    invocation: Arc<dyn SessionInvocationPort>,
    ledger: Arc<RequestLedger>,
    /// Host 操作域：按 (session incarnation, 可信连接) 惰性分配，只经
    /// `apps/list` / `apps/attach` 下发；连接 retire 即销毁（不得复用已销毁的域）。
    host_namespaces: parking_lot::Mutex<HashMap<String, RequestNamespaceId>>,
    spawner: super::task_scope::HostTaskSpawner,
}

impl SessionAppsFacade {
    pub(crate) fn new(
        session_id: impl Into<String>,
        runtime_epoch: u64,
        router: Arc<SessionInteractionRouter>,
        relay: Arc<dyn McpAppsRelayPort>,
        invocation: Arc<dyn SessionInvocationPort>,
        spawner: super::task_scope::HostTaskSpawner,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            runtime_epoch,
            router,
            relay,
            invocation,
            ledger: Arc::new(RequestLedger::new(LedgerBudget::conservative())),
            host_namespaces: parking_lot::Mutex::new(HashMap::new()),
            spawner,
        }
    }

    /// 请求账本（测试观察面；判决仍只经七方法给出）。
    #[cfg(test)]
    pub(crate) fn ledger(&self) -> &Arc<RequestLedger> {
        &self.ledger
    }

    // ── 边界校验（fail-closed；判决面之外一律 AcpError）────────────────────────

    fn ensure_session(&self, request_session_id: &str) -> Result<(), AcpError> {
        if request_session_id == self.session_id {
            Ok(())
        } else {
            // `sessionId` 只作定位：与环境不符即拒绝（不跨会话服务）。
            Err(apps_error(McpAppsErrorKind::InvalidSession))
        }
    }

    /// 请求必须来自本会话的**在线 endpoint**（仅知 sessionId 不能 attach，A9）。
    fn ensure_endpoint(&self, conn: &str) -> Result<(), AcpError> {
        if self.router.is_online_endpoint(conn) {
            Ok(())
        } else {
            Err(apps_error(McpAppsErrorKind::InvalidSession))
        }
    }

    /// Host 操作域（本连接）：不存在则分配；已销毁则显式拒绝（不重置已销毁的域）。
    fn host_namespace(&self, conn: &str) -> Result<RequestNamespaceId, AcpError> {
        if let Some(existing) = self.host_namespaces.lock().get(conn).cloned() {
            return match self.ledger.ensure_namespace(&existing, NamespaceKind::Host) {
                Ok(()) => Ok(existing),
                Err(_) => Err(apps_error(McpAppsErrorKind::UnknownNamespace)),
            };
        }
        let namespace = RequestNamespaceId::new();
        match self
            .ledger
            .ensure_namespace(&namespace, NamespaceKind::Host)
        {
            Ok(()) => {
                self.host_namespaces
                    .lock()
                    .insert(conn.to_string(), namespace.clone());
                Ok(namespace)
            }
            Err(NamespaceOpenError::CapacityExceeded) => {
                Err(apps_error(McpAppsErrorKind::CapacityExceeded))
            }
            Err(_) => Err(apps_error(McpAppsErrorKind::UnknownNamespace)),
        }
    }

    /// 请求所指附件：必须存在、纪元相符、且由**本连接**承载（旧连接无提交资格）。
    fn attachment_for_request(
        &self,
        attachment_id: &AttachmentId,
        epoch: ConnectionEpoch,
        conn: &str,
    ) -> Result<SessionAttachment, AcpError> {
        let Some(attachment) = self.router.lookup(attachment_id) else {
            // 未知 / 已销毁 / 重挂接窗口已过：判决面容错（零登记零执行）。
            return Err(verdict_error(RequestVerdict::UnknownNamespace));
        };
        if attachment.epoch() != epoch {
            // 旧纪元的提交 / 取消一律失效（§8：旧连接失去提交与取消资格）。
            return Err(apps_error(McpAppsErrorKind::StaleAttachmentEpoch));
        }
        if !attachment.is_deliverable() || attachment.connection_id() != Some(conn) {
            // 已 detach（窗口内）或由别的连接承载：本连接没有提交资格。
            return Err(apps_error(McpAppsErrorKind::Forbidden));
        }
        Ok(attachment)
    }

    /// 附件 namespace 的账本入口（已销毁 ⇒ `UnknownNamespace`，**不得**重建）。
    fn namespace_for_attachment(
        &self,
        attachment: &SessionAttachment,
    ) -> Result<RequestNamespaceId, RequestVerdict> {
        let namespace = attachment.namespace().clone();
        match self
            .ledger
            .ensure_namespace(&namespace, NamespaceKind::Attachment)
        {
            Ok(()) => Ok(namespace),
            Err(NamespaceOpenError::CapacityExceeded) => Err(RequestVerdict::CapacityRejected),
            Err(_) => Err(RequestVerdict::UnknownNamespace),
        }
    }

    // ── 七个方法 ───────────────────────────────────────────────────────────────

    /// `peri/mcp/apps/list`：当前 incarnation 内可读的轻量 descriptors + 实际能力 +
    /// 本连接的 Host 操作域。
    pub(crate) async fn list(
        &self,
        conn: &str,
        request: AppsListRequest,
    ) -> Result<AppsListResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        // 会话级可读入口顺带做到期清理：重挂接窗口已过的 detach 条目不再可见
        //（与 attach / lookup 的惰性清理同一实现，不需要后台任务）。
        let expired = self.router.expire_detached();
        if expired > 0 {
            tracing::debug!(
                session_id = %self.session_id,
                expired,
                "MCP Apps detached attachments expired"
            );
        }
        let descriptors = self
            .relay
            .list_apps(&self.session_id)
            .await
            .map_err(|error| apps_error(error.kind))?;
        let namespace = self.host_namespace(conn)?;
        Ok(AppsListResponse {
            descriptors,
            features: McpAppsFeatures::implemented(),
            limits: McpAppsLimits::conservative(),
            host_namespace: HostNamespace {
                next_seq: self.ledger.next_seq(&namespace),
                namespace,
            },
        })
    }

    /// `peri/mcp/apps/attach`：领取 record 并附着附件（**不调用任何业务工具**）。
    ///
    /// 首附分配新附件身份；`reattach` 复用该 View 的附件身份并**递增纪元**。
    pub(crate) async fn attach(
        &self,
        conn: &str,
        request: AppsAttachRequest,
    ) -> Result<AppsAttachResponse, AcpError> {
        // 重挂接复用客户端引用的附件身份（校验在 `attach_identity` 内完成：
        // 引用与本次请求必须一致，且只能落在本会话的既有附件上）；首附铸造新身份。
        let attachment_id = match request.reattach.as_ref() {
            Some(observed) => observed.attachment_id.clone(),
            None => AttachmentId::new(uuid::Uuid::now_v7().to_string()),
        };
        self.attach_identity(conn, request, attachment_id).await
    }

    async fn attach_identity(
        &self,
        conn: &str,
        request: AppsAttachRequest,
        attachment_id: AttachmentId,
    ) -> Result<AppsAttachResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        let attachment = match request.reattach.as_ref() {
            Some(observed) => {
                if observed.attachment_id != attachment_id {
                    // 客户端引用的附件身份与本次请求不一致：显式拒绝（不猜目标）。
                    return Err(apps_error(McpAppsErrorKind::Conflict));
                }
                let Some(existing) = self.router.lookup(&attachment_id) else {
                    return Err(verdict_error(RequestVerdict::UnknownNamespace));
                };
                if existing.app_id() != request.app_id {
                    return Err(apps_error(McpAppsErrorKind::Conflict));
                }
                if existing.view_key() != &request.client_view_id {
                    // 重挂接必须来自**同一个 View**：另一个页面要用首附
                    // （新附件身份），否则会把两个页面的请求域混成一个。
                    return Err(apps_error(McpAppsErrorKind::Conflict));
                }
                if existing.epoch() != observed.epoch {
                    // 客户端观察到的纪元已过时（别处已重挂接）：拒绝，不静默升级。
                    return Err(apps_error(McpAppsErrorKind::StaleAttachmentEpoch));
                }
                self.router
                    .reattach_view(&attachment_id, conn)
                    .map_err(map_attach_rejection)?
            }
            None => self
                .router
                .attach_view(ViewAttachRequest {
                    attachment_id: attachment_id.clone(),
                    app_id: request.app_id.clone(),
                    view_key: request.client_view_id.clone(),
                    connection_id: conn.to_string(),
                })
                .map_err(map_attach_rejection)?,
        };
        let namespace = self
            .ledger
            .ensure_namespace(attachment.namespace(), NamespaceKind::Attachment)
            .map(|()| attachment.namespace().clone())
            .map_err(|error| match error {
                NamespaceOpenError::CapacityExceeded => {
                    apps_error(McpAppsErrorKind::CapacityExceeded)
                }
                _ => verdict_error(RequestVerdict::UnknownNamespace),
            })?;
        let binding = AttachmentBinding {
            attachment_id: attachment.attachment_id().clone(),
            namespace: namespace.clone(),
            // 纪元 / namespace **原样取自 router**（record 与 router 必须同源）。
            connection_epoch: attachment.epoch(),
            next_seq: self.ledger.next_seq(&namespace),
        };
        let mut snapshot: AppAttachSnapshot = self
            .relay
            .attach_app(AppAttachRequest {
                session_id: self.session_id.clone(),
                app_id: request.app_id.clone(),
                owner_connection_id: conn.to_string(),
                attachment: binding.clone(),
                client_view_id: request.client_view_id.clone(),
            })
            .await
            .map_err(|error| apps_error(error.kind))?;
        // `reattached` 的**权威事实**在请求侧（本层校验过的重挂接引用）：relay 只能
        // 从交付集合近似（detach 会摘掉集合条目 ⇒ 「detach 后重挂接」在那里看不出
        // 重挂接语义）。两处取或：任一为真即重挂接。
        if request.reattach.is_some() {
            snapshot.reattached = true;
        }
        Ok(AppsAttachResponse {
            // 与 `snapshot.attachment` 同源同值（构造一次，两处共用）。
            attachment: binding,
            snapshot,
            features: McpAppsFeatures::implemented(),
            limits: McpAppsLimits::conservative(),
        })
    }

    /// `peri/mcp/apps/request`：登记 + 受限并发执行（同 key 只执行一次）。
    pub(crate) async fn request(
        self: &Arc<Self>,
        conn: &str,
        request: AppsRequestRequest,
    ) -> Result<AppsRequestResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        let attachment =
            self.attachment_for_request(&request.attachment_id, request.epoch, conn)?;
        let namespace = match self.namespace_for_attachment(&attachment) {
            Ok(namespace) => namespace,
            Err(verdict) => return Ok(unexecuted_response(verdict, &self.ledger, None)),
        };
        let app_id = attachment.app_id().to_string();
        let (operation, plan) = match &request.operation {
            AppsOperation::ToolsCall { name, arguments } => {
                // App 回调的世代自述必须等于 record 的世代（service 会复核）：
                // 记录事实只经 relay 的 App facts 面取得（本层不猜测）。
                let server_generation = self.record_generation(&app_id).await?;
                (
                    RequestOperation::AppToolsCall {
                        record_id: app_id.clone(),
                        tool_name: name.clone(),
                        arguments: arguments.clone(),
                    },
                    OperationPlan::AppToolsCall {
                        record_id: app_id.clone(),
                        server_generation,
                        attachment_id: request.attachment_id.clone(),
                        epoch: request.epoch,
                        tool_name: name.clone(),
                        arguments: arguments.clone(),
                    },
                )
            }
            AppsOperation::ResourcesRead { uri } => (
                RequestOperation::ResourcesRead {
                    record_id: app_id.clone(),
                    uri: uri.clone(),
                },
                OperationPlan::ResourcesRead {
                    app_id: app_id.clone(),
                    attachment_id: request.attachment_id.clone(),
                    epoch: request.epoch,
                    uri: uri.clone(),
                },
            ),
            AppsOperation::ResourcesList { cursor } => (
                RequestOperation::ResourcesList {
                    record_id: app_id.clone(),
                    cursor: cursor.clone(),
                },
                OperationPlan::ResourcesList {
                    app_id: app_id.clone(),
                    attachment_id: request.attachment_id.clone(),
                    epoch: request.epoch,
                    cursor: cursor.clone(),
                },
            ),
        };
        let fingerprint = RequestFingerprint::new(&self.session_id, &operation);
        match self.ledger.admit(&namespace, request.seq, &fingerprint) {
            AdmissionVerdict::Accepted { pending } => {
                let executed = self.spawn_operation(
                    namespace.clone(),
                    request.seq,
                    Arc::clone(&pending),
                    plan,
                );
                Ok(AppsRequestResponse {
                    verdict: RequestVerdict::Accepted,
                    admitted: true,
                    outcome: Some(if executed {
                        AppsRequestOutcome::Pending
                    } else {
                        // 宿主停止受理新任务 ⇒ 从未成为 invocation ⇒ 显式未执行。
                        not_executed_outcome()
                    }),
                    next_seq: self.ledger.next_seq(&namespace),
                })
            }
            AdmissionVerdict::JoinedPending { pending } => Ok(AppsRequestResponse {
                verdict: RequestVerdict::JoinedPending,
                admitted: false,
                // 加入同一等待：终态返回同一结果，不重复审批 / 执行。
                outcome: Some(
                    pending
                        .slot
                        .wait(std::time::Duration::from_millis(REQUEST_WAIT_TIMEOUT_MS))
                        .await
                        .unwrap_or(AppsRequestOutcome::Pending),
                ),
                next_seq: self.ledger.next_seq(&namespace),
            }),
            AdmissionVerdict::Duplicate { outcome } => Ok(AppsRequestResponse {
                verdict: RequestVerdict::Duplicate,
                admitted: false,
                outcome: Some(outcome),
                next_seq: self.ledger.next_seq(&namespace),
            }),
            other => Ok(unexecuted_response(
                admission_verdict(&other),
                &self.ledger,
                Some(&namespace),
            )),
        }
    }

    /// `peri/mcp/apps/cancel`：只命中该次操作（`accepted` 不代表远端已回滚）。
    pub(crate) async fn cancel(
        &self,
        conn: &str,
        request: AppsCancelRequest,
    ) -> Result<AppsCancelResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        let attachment =
            self.attachment_for_request(&request.attachment_id, request.epoch, conn)?;
        let verdict = self.ledger.cancel(
            attachment.namespace(),
            request.seq,
            RequestCancelReason::ClientCancelled,
        );
        Ok(AppsCancelResponse { verdict })
    }

    /// `peri/mcp/apps/detach`：幂等断开 View（有界窗口内可重挂接；**不撤销 record**、
    /// 不取消已 dispatch 的调用）。
    pub(crate) async fn detach(
        &self,
        conn: &str,
        request: AppsDetachRequest,
    ) -> Result<AppsDetachResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        let Some(attachment) = self.router.lookup(&request.attachment_id) else {
            return Ok(AppsDetachResponse {
                verdict: DetachVerdict::UnknownAttachment,
            });
        };
        if attachment.epoch() != request.epoch {
            return Err(apps_error(McpAppsErrorKind::StaleAttachmentEpoch));
        }
        if attachment.is_deliverable() && attachment.connection_id() != Some(conn) {
            // 附着中的附件只允许其**承载连接**撤销投递资格（别的连接不能替它 detach）。
            // 已 detach 的附件不再有承载连接：幂等判决（AlreadyDetached）由 router 给出。
            return Err(apps_error(McpAppsErrorKind::Forbidden));
        }
        let verdict = self
            .router
            .detach(&request.attachment_id, AttachmentDetachReason::ViewDetached);
        // 交付资格同步撤销（relay 侧）；record 本体与已 dispatch 的执行都不受影响。
        self.relay
            .detach_attachment(&request.attachment_id, AttachmentDetachReason::ViewDetached);
        // 未决请求收到「准入前放弃」信号：从未 dispatch 的请求由账本出显式未执行，
        // 已在 canonical 管线内的请求保持它自己的 outcome（**不**改写）。
        self.ledger.cancel_pending(
            attachment.namespace(),
            RequestCancelReason::AttachmentReplaced,
        );
        Ok(AppsDetachResponse {
            verdict: match verdict {
                RouterDetachVerdict::Detached => DetachVerdict::Detached,
                RouterDetachVerdict::AlreadyDetached => DetachVerdict::AlreadyDetached,
                RouterDetachVerdict::UnknownAttachment => DetachVerdict::UnknownAttachment,
            },
        })
    }

    /// `peri/mcp/apps/close`：结束该 App 的交互资格（**不删模型历史**、不取消已
    /// dispatch 的执行），并销毁其附件 namespace —— 同 key 重传因此不能取回
    /// close 之前的成功结果（裁决 D9 的附加要求）。
    pub(crate) async fn close(
        &self,
        conn: &str,
        request: AppsCloseRequest,
    ) -> Result<AppsCloseResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        // 先取 router 侧附件（含 namespace 权威值），再终态撤销：撤销后无法反查。
        let attachments = self.router.attachments_for_app(&request.app_id);
        let outcome = self.relay.close_app(&self.session_id, &request.app_id);
        for attachment in &attachments {
            self.ledger.cancel_pending(
                attachment.namespace(),
                RequestCancelReason::AttachmentReplaced,
            );
            self.ledger.destroy(attachment.namespace());
        }
        let removed = self.router.close_app_attachments(&request.app_id);
        let verdict = if matches!(
            outcome.verdict,
            peri_acp_types::mcp_apps_facade::CloseVerdict::Closed
        ) || !removed.is_empty()
        {
            peri_acp_types::mcp_apps_facade::CloseVerdict::Closed
        } else {
            peri_acp_types::mcp_apps_facade::CloseVerdict::AlreadyClosed
        };
        Ok(AppsCloseResponse {
            verdict,
            detached_attachments: outcome.detached_attachments,
        })
    }

    /// `peri/mcp/apps/rerun`：Host 发起的新用户意图。
    ///
    /// 目标**只**来自 `sourceInvocationRef` 的服务端记录（无回落）；同 key 重传
    /// 复用结果且不重复审批；新 key ⇒ 重新准入 + 审批（D9）。
    pub(crate) async fn rerun(
        self: &Arc<Self>,
        conn: &str,
        request: AppsRerunRequest,
    ) -> Result<AppsRequestResponse, AcpError> {
        self.ensure_session(&request.session_id)?;
        self.ensure_endpoint(conn)?;
        let namespace = self.host_namespace(conn)?;
        if namespace != request.host_namespace {
            // 只能用**本连接**的 Host 域（跨连接 / 陈旧域一律拒绝）。
            return Err(apps_error(McpAppsErrorKind::Forbidden));
        }
        let target = self
            .relay
            .source_target(&self.session_id, &request.source_invocation_ref)
            .map_err(|error| apps_error(error.kind))?;
        let operation = RequestOperation::HostToolsCall {
            server_id: target.server_id.clone(),
            tool_name: target.wire_tool_name.clone(),
            arguments: request.arguments.clone(),
        };
        let fingerprint = RequestFingerprint::new(&self.session_id, &operation);
        match self.ledger.admit(&namespace, request.seq, &fingerprint) {
            AdmissionVerdict::Accepted { pending } => {
                let executed = self.spawn_operation(
                    namespace.clone(),
                    request.seq,
                    Arc::clone(&pending),
                    OperationPlan::HostToolsCall {
                        server_id: target.server_id,
                        tool_name: target.wire_tool_name,
                        arguments: request.arguments,
                    },
                );
                Ok(AppsRequestResponse {
                    verdict: RequestVerdict::Accepted,
                    admitted: true,
                    outcome: Some(if executed {
                        AppsRequestOutcome::Pending
                    } else {
                        not_executed_outcome()
                    }),
                    next_seq: self.ledger.next_seq(&namespace),
                })
            }
            AdmissionVerdict::JoinedPending { pending } => Ok(AppsRequestResponse {
                verdict: RequestVerdict::JoinedPending,
                admitted: false,
                // 同 key 重传：复用同一结果，**不重复审批**（D9）。
                outcome: Some(
                    pending
                        .slot
                        .wait(std::time::Duration::from_millis(REQUEST_WAIT_TIMEOUT_MS))
                        .await
                        .unwrap_or(AppsRequestOutcome::Pending),
                ),
                next_seq: self.ledger.next_seq(&namespace),
            }),
            AdmissionVerdict::Duplicate { outcome } => Ok(AppsRequestResponse {
                verdict: RequestVerdict::Duplicate,
                admitted: false,
                outcome: Some(outcome),
                next_seq: self.ledger.next_seq(&namespace),
            }),
            other => Ok(unexecuted_response(
                admission_verdict(&other),
                &self.ledger,
                Some(&namespace),
            )),
        }
    }

    /// Host 归属值（`{namespace, seq}`）：本连接的 Host 操作域 + 账本推进的序号。
    ///
    /// 旧 `peri/mcp/invoke` 别名用（遗留 wire 语义：客户端字段即目标）；新方法
    /// `apps/rerun` **不**经此处（它的目标只从服务端记录导出，裁决 D4）。
    pub(crate) fn host_attribution(
        &self,
        conn: &str,
    ) -> Result<peri_acp_types::mcp_apps::HostInvocationRef, AcpError> {
        let namespace = self.host_namespace(conn)?;
        let seq = self.ledger.issue_legacy_seq(&namespace);
        Ok(peri_acp_types::mcp_apps::HostInvocationRef {
            namespace: namespace.to_string(),
            seq: seq.get(),
        })
    }

    /// 旧 `peri/mcp/open` 的 token→record 解析（§A 运行中裁决）：
    /// `invocation_token` 与签发点的 `model.invocation_id` 同源（`tool_bridge.rs`），
    /// 只经 `list_apps` 的 descriptor 匹配——**不新增 token→record 索引状态**，
    /// 匹配失败即显式拒绝（不回落、不猜）。
    pub(crate) async fn record_for_invocation_token(
        &self,
        session_id: &str,
        invocation_token: &str,
        server_id: &str,
        tool_name: &str,
    ) -> Result<AliasRecordFacts, peri_acp_types::mcp_apps::McpAppsRelayError> {
        self.ensure_session(session_id).map_err(|_| {
            peri_acp_types::mcp_apps::McpAppsRelayError {
                kind: McpAppsErrorKind::InvalidSession,
            }
        })?;
        let descriptor = self
            .relay
            .list_apps(&self.session_id)
            .await?
            .into_iter()
            .find(|descriptor| {
                descriptor.model.invocation_id.as_str() == invocation_token
                    && descriptor.server_id == server_id
                    && descriptor.instantiating_tool == tool_name
            })
            .ok_or(peri_acp_types::mcp_apps::McpAppsRelayError {
                kind: McpAppsErrorKind::PolicyDenied,
            })?;
        let mcp_protocol_version = self
            .relay
            .binding_protocol_version(&descriptor.app_id)
            .await?;
        Ok(AliasRecordFacts {
            descriptor,
            mcp_protocol_version,
        })
    }

    // ── 执行 ──────────────────────────────────────────────────────────────────

    /// record 的当前世代（App 回调的世代自述来源；缺记录 ⇒ 0，由 service 判
    /// `TargetRevoked`，本层不代它下结论）。
    async fn record_generation(&self, app_id: &str) -> Result<u64, AcpError> {
        let descriptors = self
            .relay
            .list_apps(&self.session_id)
            .await
            .map_err(|error| apps_error(error.kind))?;
        Ok(descriptors
            .into_iter()
            .find(|descriptor| descriptor.app_id == app_id)
            .map(|descriptor| descriptor.server_generation)
            .unwrap_or(0))
    }

    /// 受限并发执行（`Accepted` 之后的唯一执行入口）。
    ///
    /// 返回 `false` = 宿主已停止受理新任务（请求从未成为 invocation）；调用方必须
    /// 按「显式未执行」处理。
    fn spawn_operation(
        self: &Arc<Self>,
        namespace: RequestNamespaceId,
        seq: RequestSeq,
        pending: Arc<PendingRequest>,
        plan: OperationPlan,
    ) -> bool {
        let facade = Arc::clone(self);
        let spawn = self.spawner.spawn(
            super::task_scope::HostTaskOwnerKind::Connection,
            super::task_scope::HostTaskKind::McpAppsRelay,
            async move {
                facade.run_operation(namespace, seq, pending, plan).await;
            },
        );
        match spawn {
            Ok(()) => true,
            Err(closed) => {
                tracing::warn!(
                    session_id = %self.session_id,
                    ?closed,
                    "MCP Apps request was not admitted to an execution slot (host scope closed)"
                );
                false
            }
        }
    }

    async fn run_operation(
        self: Arc<Self>,
        namespace: RequestNamespaceId,
        seq: RequestSeq,
        pending: Arc<PendingRequest>,
        plan: OperationPlan,
    ) {
        if pending.cancel.is_cancelled() {
            // 从未成为 invocation 的请求：账本侧出显式未执行（裁决 D7），零 wire。
            self.publish(&namespace, seq, &pending, not_executed_outcome());
            return;
        }
        // 取消令牌只作准入前放弃信号（service 的提交后执行不读它）：
        // 这里把它接线到 canonical 请求，并让 service 自行产出终态事实。
        let cancel = pending.cancel.clone();
        let outcome = match plan {
            OperationPlan::AppToolsCall {
                record_id,
                server_generation,
                attachment_id,
                epoch,
                tool_name,
                arguments,
            } => {
                let request = SessionInvocationRequest {
                    session_id: self.session_id.clone(),
                    runtime_epoch: self.runtime_epoch,
                    origin: InvocationOrigin::App {
                        app_id: record_id.clone(),
                        attachment_id: Some(attachment_id),
                        connection_epoch: Some(epoch),
                        server_generation,
                    },
                    target: InvocationTarget::App {
                        record_id,
                        tool_name,
                        arguments,
                    },
                    cancel,
                    invocation_id: None,
                };
                self.tools_call_outcome(request).await
            }
            OperationPlan::HostToolsCall {
                server_id,
                tool_name,
                arguments,
            } => {
                let request = SessionInvocationRequest {
                    session_id: self.session_id.clone(),
                    runtime_epoch: self.runtime_epoch,
                    origin: InvocationOrigin::Host {
                        namespace: namespace.to_string(),
                        seq: seq.get(),
                    },
                    target: InvocationTarget::Host {
                        server_id,
                        tool_name,
                        arguments,
                    },
                    cancel,
                    invocation_id: None,
                };
                self.tools_call_outcome(request).await
            }
            // 资源读取只发给 record 绑定的 server（Peri 不自行 fetch URL / 读本机文件）。
            OperationPlan::ResourcesRead {
                app_id,
                attachment_id,
                epoch,
                uri,
            } => match self
                .relay
                .read_resources(ResourceReadRequest {
                    session_id: self.session_id.clone(),
                    app_id,
                    attachment_id,
                    epoch,
                    uri,
                })
                .await
            {
                Ok(resources) => AppsRequestOutcome::ResourcesRead { resources },
                Err(error) => resource_failure(error.kind),
            },
            OperationPlan::ResourcesList {
                app_id,
                attachment_id,
                epoch,
                cursor,
            } => match self
                .relay
                .list_resources(ResourceListRequest {
                    session_id: self.session_id.clone(),
                    app_id,
                    attachment_id,
                    epoch,
                    cursor,
                })
                .await
            {
                Ok(list) => AppsRequestOutcome::ResourcesList {
                    resources: list.resources,
                    next_cursor: list.next_cursor,
                },
                Err(error) => resource_failure(error.kind),
            },
        };
        self.publish(&namespace, seq, &pending, outcome);
    }

    /// canonical 调用的终态事实（service 的结论原样转述，本层不改写）。
    async fn tools_call_outcome(&self, request: SessionInvocationRequest) -> AppsRequestOutcome {
        match self.invocation.invoke(request).await {
            Ok(outcome) => AppsRequestOutcome::ToolsCall(outcome),
            Err(error) => {
                // `Err` = 该请求无法作为一次调用被受理（无执行身份、零 wire）。
                tracing::warn!(
                    session_id = %self.session_id,
                    kind = error.kind.as_str(),
                    "MCP Apps canonical invocation was not accepted"
                );
                AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::OutcomeUnknown {
                    reason: format!("canonical invocation rejected: {}", error.kind.as_str()),
                })
            }
        }
    }

    fn publish(
        &self,
        namespace: &RequestNamespaceId,
        seq: RequestSeq,
        pending: &Arc<PendingRequest>,
        outcome: AppsRequestOutcome,
    ) {
        if !self.ledger.complete(namespace, seq, outcome.clone()) {
            // namespace 已销毁 / 已终结：结果无处记账，但等待者仍必须解除等待。
            pending.slot.complete(outcome);
        }
    }

    /// 连接 retire：销毁该连接的 Host 操作域（附件 namespace 留在重挂接窗口内，
    /// 不销毁——窗口内的重挂接必须能继续用同一 namespace 取回结果）。
    pub(crate) fn on_connection_retired(&self, conn: &str) {
        let namespace = self.host_namespaces.lock().remove(conn);
        if let Some(namespace) = namespace {
            self.ledger
                .cancel_pending(&namespace, RequestCancelReason::AttachmentReplaced);
            self.ledger.destroy(&namespace);
        }
    }

    /// 会话关闭：销毁本层持有的全部 Host 域（附件 namespace 随环境丢弃；
    /// 在飞执行由 invocation owner 收尾）。
    pub(crate) fn on_session_closing(&self) {
        let namespaces: Vec<RequestNamespaceId> = self
            .host_namespaces
            .lock()
            .drain()
            .map(|(_, id)| id)
            .collect();
        for namespace in namespaces {
            self.ledger
                .cancel_pending(&namespace, RequestCancelReason::AttachmentReplaced);
            self.ledger.destroy(&namespace);
        }
    }
}

fn admission_verdict(verdict: &AdmissionVerdict) -> RequestVerdict {
    match verdict {
        AdmissionVerdict::Accepted { .. } => RequestVerdict::Accepted,
        AdmissionVerdict::JoinedPending { .. } => RequestVerdict::JoinedPending,
        AdmissionVerdict::Duplicate { .. } => RequestVerdict::Duplicate,
        AdmissionVerdict::Conflict => RequestVerdict::Conflict,
        AdmissionVerdict::ResultExpired => RequestVerdict::ResultExpired,
        AdmissionVerdict::SequenceGap { expected } => RequestVerdict::SequenceGap {
            expected: *expected,
        },
        AdmissionVerdict::CapacityRejected => RequestVerdict::CapacityRejected,
        AdmissionVerdict::UnknownNamespace => RequestVerdict::UnknownNamespace,
    }
}

fn unexecuted_response(
    verdict: RequestVerdict,
    ledger: &RequestLedger,
    namespace: Option<&RequestNamespaceId>,
) -> AppsRequestResponse {
    AppsRequestResponse {
        verdict,
        admitted: verdict.is_admitted(),
        outcome: None,
        next_seq: namespace
            .map(|namespace| ledger.next_seq(namespace))
            .unwrap_or_else(|| RequestSeq::new(1)),
    }
}

fn not_executed_outcome() -> AppsRequestOutcome {
    AppsRequestOutcome::ToolsCall(not_executed_before_commit())
}

/// 资源操作失败：显式未执行（4a 没有资源级的 `NotExecutedCause` 变体，也不新增；
/// 失败发生在任何 wire 调用之前，绝不呈现为成功）。
fn resource_failure(kind: McpAppsErrorKind) -> AppsRequestOutcome {
    tracing::debug!(
        kind = kind.as_str(),
        "MCP Apps resource operation failed before any side effect"
    );
    AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::NotExecuted {
        cause: NotExecutedCause::TargetRevoked,
    })
}

fn map_attach_rejection(rejection: AttachRejection) -> AcpError {
    match rejection {
        AttachRejection::EndpointUnavailable => apps_error(McpAppsErrorKind::InvalidSession),
        AttachRejection::TableFull => apps_error(McpAppsErrorKind::CapacityExceeded),
        AttachRejection::AttachmentExists => apps_error(McpAppsErrorKind::Conflict),
        AttachRejection::UnknownAttachment => verdict_error(RequestVerdict::UnknownNamespace),
    }
}

#[path = "apps_facade_alias.rs"]
mod alias;

#[cfg(test)]
#[path = "apps_facade_test.rs"]
mod tests;
