//! MCP Apps relay：连接绑定校验 + 资源读取 + 把 App 回调 / Host 调用交给
//! **session-owned canonical 入口**（[`SessionInvocationPort`]）。
//!
//! 本层不再持有 dispatcher 或执行取消令牌（设计 §4 删除清单 3）：App 记录是纯数据，
//! 调用经 [`crate::mcp::session_invocation::SessionInvocationService`] 重新取用。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    interaction::{AttachmentId, ConnectionEpoch},
    mcp_apps::{
        McpAppInvokeOutcome, McpAppInvokeRequest, McpAppsErrorKind, McpAppsRelayError,
        McpAppsRelayPort, RawResource, SessionInvocationPort, MCP_APPS_HTML_MIME,
    },
    mcp_apps_facade::{
        AppAttachRequest, AppAttachSnapshot, AppDescriptor, AppExecutionState,
        AttachmentDetachReason, CloseAppOutcome, CloseVerdict, McpAppsFeatures, McpAppsLimits,
        RawResourceDescriptor, ResourceList, ResourceListRequest, ResourceReadRequest,
        TargetIdentity, UiResourceRef,
    },
};

use super::{
    apps::{AttachmentRoute, McpAppRecord, RecordState},
    client::McpClientPool,
};

/// Production MCP Apps relay backed by the deployment-scoped pool.
///
/// 资源 / 目录读取与 App 调用共用同一 deployment pool；工具调用一律经 session-owned
/// canonical 入口（未装配该入口时显式 fail-closed，见 [`Self::session_invocation`]）。
pub struct PoolMcpAppsRelay {
    pub(crate) pool: Arc<McpClientPool>,
    /// session-owned canonical 调用入口（装配期注入，与 service 共享同一 runtime
    /// epoch）。
    ///
    /// **必填**：App 回调与 Host invoke 都只能经这一条入口执行；不存在「未装配
    /// 入口」的运行时状态（装配点在 `host/workspace.rs` 的会话环境装配，与池
    /// 同一作用域）。缺失即构造失败，而不是请求期 fail-closed 分支。
    session_invocation: (u64, Arc<dyn SessionInvocationPort>),
}

impl PoolMcpAppsRelay {
    pub fn new(
        pool: Arc<McpClientPool>,
        runtime_epoch: u64,
        port: Arc<dyn SessionInvocationPort>,
    ) -> Self {
        Self {
            pool,
            session_invocation: (runtime_epoch, port),
        }
    }

    fn handle(
        &self,
        server_id: &str,
    ) -> Result<Arc<super::client::McpClientHandle>, McpAppsRelayError> {
        self.pool
            .get_client(server_id)
            .filter(|handle| handle.peer.is_some())
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))
    }

    /// 登记本会话的运行时世代（签发点据此给 App record 盖章）。
    ///
    /// **装配点调用一次**（会话环境装配，与 relay 同一 epoch / 同一作用域）：
    /// 未登记时 MCP bridge 不签发 App record（显式告警，不猜世代）。
    /// 会话关闭（`close_session`）清除登记。
    pub fn register_session_incarnation(&self, session_id: &str) {
        self.pool
            .app_binding_leases
            .register_session_incarnation(session_id, *self.invocation().0);
    }

    /// session-owned 入口（装配期注入；与 service 共享同一 runtime epoch）。
    pub(super) fn invocation(&self) -> (&u64, &Arc<dyn SessionInvocationPort>) {
        (&self.session_invocation.0, &self.session_invocation.1)
    }

    /// 记录 → descriptor 投影（`apps/list` 的列表项）。
    ///
    /// 执行状态是**当前可执行性**的事实投影：不属本 incarnation / 已撤销 ⇒ `Closed`；
    /// server 世代不再是池的当前世代（或已无可观察句柄）⇒ `GenerationUnavailable`。
    fn descriptor(&self, record: &McpAppRecord) -> AppDescriptor {
        let observed = self
            .pool
            .get_client(&record.server_id)
            .map(|handle| self.pool.handle_generation(&handle));
        let execution_state = if record.state() == RecordState::Revoked
            || record.incarnation != self.runtime_epoch()
        {
            AppExecutionState::Closed
        } else if observed != Some(record.server_generation) {
            AppExecutionState::GenerationUnavailable {
                bound: record.server_generation,
                // 0 = 当前没有可观察的 server 句柄（世代从 1 起，0 不表示真实世代）。
                observed: observed.unwrap_or(0),
            }
        } else {
            AppExecutionState::Live
        };
        AppDescriptor {
            app_id: record.record_id.clone(),
            server_id: record.server_id.clone(),
            server_generation: record.server_generation,
            resource_uri: record.resource_uri.clone(),
            instantiating_tool: record.instantiating_tool.clone(),
            qualification: record.qualification.clone(),
            model: record.model().clone(),
            execution_state,
            attachments: record.attachment_summaries(),
            retention_remaining_ms: record.retention_remaining().as_millis() as u64,
        }
    }

    /// 附件级复核：会话 / 记录 / 附件 / 纪元 / server 世代全部一致才返回绑定。
    ///
    /// 任一不符都是**显式拒绝**（不降级、不回落）：纪元不符 ⇒
    /// [`McpAppsErrorKind::StaleAttachmentEpoch`]（调用方必须重新 attach）。
    fn bound_attachment(
        &self,
        session_id: &str,
        app_id: &str,
        attachment_id: &AttachmentId,
        epoch: ConnectionEpoch,
    ) -> Result<
        (
            Arc<McpAppRecord>,
            Arc<AttachmentRoute>,
            Arc<super::client::McpClientHandle>,
        ),
        McpAppsRelayError,
    > {
        let record = self
            .pool
            .app_binding_leases
            .find(app_id)
            .filter(|record| record.owner_session_id == session_id)
            .ok_or_else(|| relay_error(McpAppsErrorKind::PolicyDenied))?;
        if record.state() == RecordState::Revoked {
            return Err(relay_error(McpAppsErrorKind::PolicyDenied));
        }
        let route = record
            .attachment_for(attachment_id)
            .ok_or_else(|| relay_error(McpAppsErrorKind::StaleAttachmentEpoch))?;
        if route.connection_epoch != epoch {
            return Err(relay_error(McpAppsErrorKind::StaleAttachmentEpoch));
        }
        let handle = self.handle(&record.server_id)?;
        let _permit = handle
            .call_gate
            .try_acquire()
            .map_err(|_| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        if self.pool.handle_generation(&handle) != record.server_generation {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        Ok((record, route, handle))
    }
}

#[async_trait]
impl McpAppsRelayPort for PoolMcpAppsRelay {
    fn close_connection(&self, owner_connection_id: &str) {
        // 裁决 D6：连接关闭 = retire endpoint（由 router 置离线）+ 该连接附件
        // detach（有界重挂接窗口）。**不在本层撤销 record**：record 与其模型历史
        // 保留，终态撤销只在 `apps/close` / session close / incarnation 换代 /
        // 保留到期发生。已 dispatch 的调用不被本操作取消（由 invocation owner 收尾）。
        let detached = self
            .pool
            .app_binding_leases
            .detach_connection(owner_connection_id);
        let (_, port) = self.invocation();
        for attachment_id in &detached {
            port.revoke_attachment(attachment_id);
        }
        tracing::debug!(
            owner_connection_id,
            detached = detached.len(),
            "MCP Apps connection attachments detached"
        );
    }

    fn close_session(&self, owner_session_id: &str) {
        self.pool
            .app_binding_leases
            .revoke_session(owner_session_id);
        let (_, port) = self.invocation();
        port.session_closing();
    }

    fn runtime_epoch(&self) -> u64 {
        *self.invocation().0
    }

    async fn list_apps(&self, session_id: &str) -> Result<Vec<AppDescriptor>, McpAppsRelayError> {
        Ok(self
            .pool
            .app_binding_leases
            .records_for_session(session_id)
            .iter()
            .map(|record| self.descriptor(record))
            .collect())
    }

    async fn attach_app(
        &self,
        request: AppAttachRequest,
    ) -> Result<AppAttachSnapshot, McpAppsRelayError> {
        let record = self
            .pool
            .app_binding_leases
            .find(&request.app_id)
            .filter(|record| record.owner_session_id == request.session_id)
            .filter(|record| record.state() != RecordState::Revoked)
            .ok_or_else(|| relay_error(McpAppsErrorKind::PolicyDenied))?;
        // 记录必须属于**当前** incarnation：旧环境的记录不得附着（fail-closed）。
        if record.incarnation != self.runtime_epoch() {
            return Err(relay_error(McpAppsErrorKind::StaleAttachmentEpoch));
        }
        // 绑定 server 必须是池内当前世代（否则 App 没有可执行面）。
        let handle = self.handle(&record.server_id)?;
        let _permit = handle
            .call_gate
            .try_acquire()
            .map_err(|_| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        if self.pool.handle_generation(&handle) != record.server_generation {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        // 重挂接语义 = 「该 record 此前已有附件」：**本次调用之前**交付集合非空，
        // **或**本条附件身份此前已在记录里（`rebind`），**或**该记录曾被附着过
        // （`ever_attached`：detach 摘掉集合条目后，新 View 首附同样是重挂接）。
        // 三者的并集覆盖「detach 后重挂接」的全部形状；`ever_attached` 是有界记忆位，
        // 不参与准入与判决。
        let had_attachments = !record.attachments().is_empty() || record.ever_attached();
        // 纪元 / namespace / 页面身份由**调用方**（router）给出：本层不铸造纪元。
        let route = Arc::new(AttachmentRoute::new(
            request.owner_connection_id,
            request.attachment.attachment_id.clone(),
            request.attachment.namespace.clone(),
            request.attachment.connection_epoch,
            request.client_view_id,
        ));
        let rebound = record.rebind_attachment(Arc::clone(&route));
        if !rebound && !record.attach(route) {
            // 已撤销在上面已排除 ⇒ 剩余原因只有附件数上限 / 重复附件身份。
            return Err(relay_error(McpAppsErrorKind::CapacityExceeded));
        }
        let reattached = rebound || had_attachments;
        Ok(AppAttachSnapshot {
            descriptor: self.descriptor(&record),
            // 序号由调用方（ledger 当前推进值）给出：附着快照不是第二权威。
            attachment: request.attachment,
            ui_resource: UiResourceRef {
                uri: record.resource_uri.clone(),
                mime_type: MCP_APPS_HTML_MIME.to_string(),
            },
            initial_input: record.effective_input.clone(),
            // 完整 raw（含 `isError:true`）：attach 不调用业务工具，也不补齐内容。
            initial_result: record.initial_result().as_ref().clone(),
            features: McpAppsFeatures::implemented(),
            limits: McpAppsLimits::conservative(),
            reattached,
        })
    }

    fn detach_attachment(&self, attachment_id: &AttachmentId, reason: AttachmentDetachReason) {
        let Some(record) = self
            .pool
            .app_binding_leases
            .record_for_attachment(attachment_id)
        else {
            tracing::debug!(
                attachment_id = %attachment_id,
                ?reason,
                "MCP Apps detach ignored: attachment not found"
            );
            return;
        };
        if record.detach_attachment(attachment_id) {
            let (_, port) = self.invocation();
            port.revoke_attachment(attachment_id);
        }
        tracing::debug!(
            attachment_id = %attachment_id,
            app_id = %record.record_id,
            ?reason,
            "MCP Apps attachment detached"
        );
    }

    fn close_app(&self, session_id: &str, app_id: &str) -> CloseAppOutcome {
        let record = self
            .pool
            .app_binding_leases
            .find(app_id)
            .filter(|record| record.owner_session_id == session_id);
        let Some(record) = record else {
            // 幂等：本会话没有这个 App（已关闭或从未存在）。
            return CloseAppOutcome {
                verdict: CloseVerdict::AlreadyClosed,
                detached_attachments: Vec::new(),
            };
        };
        let attachments: Vec<AttachmentId> = record
            .attachments()
            .iter()
            .map(|route| route.attachment_id.clone())
            .collect();
        if !self.pool.app_binding_leases.revoke_app(app_id) {
            return CloseAppOutcome {
                verdict: CloseVerdict::AlreadyClosed,
                detached_attachments: Vec::new(),
            };
        }
        let (_, port) = self.invocation();
        // 结束交互资格：撤销 record 与附件交付资格；已 dispatch 的调用不受影响。
        port.revoke_app(app_id);
        for attachment_id in &attachments {
            port.revoke_attachment(attachment_id);
        }
        CloseAppOutcome {
            verdict: CloseVerdict::Closed,
            detached_attachments: attachments,
        }
    }

    async fn read_resources(
        &self,
        request: ResourceReadRequest,
    ) -> Result<Vec<RawResource>, McpAppsRelayError> {
        let (record, _route, handle) = self.bound_attachment(
            &request.session_id,
            &request.app_id,
            &request.attachment_id,
            request.epoch,
        )?;
        // 读取始终发给**绑定** server：Peri 不按 URI 自行 fetch URL 或读本机文件。
        // UI 文档（`ui://` + HTML MIME + text/blob 二选一）的额外校验在 ACP handler
        // 层保留（`host/mcp_apps.rs`），本层不重复也不放宽。
        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        let result = peer
            .read_resource(rmcp::model::ReadResourceRequestParams::new(
                request.uri.clone(),
            ))
            .await
            .map_err(|_| relay_error(McpAppsErrorKind::ResourceNotFound))?;
        let raw: Vec<RawResource> = serde_json::to_value(result)
            .ok()
            .and_then(|value| value.get("contents")?.as_array().cloned())
            .and_then(|values| {
                values
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .filter(|values| !values.is_empty())
            .ok_or_else(|| relay_error(McpAppsErrorKind::InvalidResource))?;
        tracing::debug!(
            server_id = %record.server_id,
            uri = %request.uri,
            contents = raw.len(),
            "MCP Apps resource read"
        );
        Ok(raw)
    }

    async fn list_resources(
        &self,
        request: ResourceListRequest,
    ) -> Result<ResourceList, McpAppsRelayError> {
        let (_record, _route, handle) = self.bound_attachment(
            &request.session_id,
            &request.app_id,
            &request.attachment_id,
            request.epoch,
        )?;
        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        let params =
            rmcp::model::PaginatedRequestParams::default().with_cursor(request.cursor.clone());
        let result = peer
            .list_resources(Some(params))
            .await
            .map_err(|_| relay_error(McpAppsErrorKind::UpstreamProtocolError))?;
        // membership 不是准入依据：目录项按 server 原样转述，未知扩展字段保真。
        let resources: Vec<RawResourceDescriptor> = result
            .resources
            .into_iter()
            .filter_map(|resource| {
                serde_json::from_value(serde_json::to_value(resource).ok()?).ok()
            })
            .collect();
        Ok(ResourceList {
            resources,
            next_cursor: result.next_cursor,
        })
    }

    fn source_target(
        &self,
        session_id: &str,
        source: &peri_acp_types::tool_invocation::InvocationId,
    ) -> Result<TargetIdentity, McpAppsRelayError> {
        // rerun 的目标权威来自 canonical 入口保留的 outcome 记录（`target`）。
        // 查不到 / 会话不符 / 该次调用未绑定目标：**显式拒绝，无回落**——绝不按
        // 客户端字段执行（裁决 D4）。
        let (_, port) = self.invocation();
        port.source_target(session_id, source).ok_or_else(|| {
            tracing::warn!(
                session_id,
                invocation_id = %source,
                "MCP Apps rerun target unavailable: no retained outcome record carries a target"
            );
            relay_error(McpAppsErrorKind::PolicyDenied)
        })
    }

    async fn binding_protocol_version(&self, app_id: &str) -> Result<String, McpAppsRelayError> {
        // 按**记录身份**复核（S8）：record 不存在 / 已撤销 ⇒ `PolicyDenied`；
        // server 世代不再是池内当前世代 ⇒ `StaleServerGeneration`；无连接 ⇒
        // `ServerDisconnected`。成功返回该连接协商出的 MCP 协议版本。
        let record = self
            .pool
            .app_binding_leases
            .find(app_id)
            .filter(|record| record.state() != RecordState::Revoked)
            .ok_or_else(|| relay_error(McpAppsErrorKind::PolicyDenied))?;
        let handle = self.handle(&record.server_id)?;
        let _permit = handle
            .call_gate
            .try_acquire()
            .map_err(|_| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        if self.pool.handle_generation(&handle) != record.server_generation {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .map(|info| info.protocol_version.to_string())
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))
    }

    async fn invoke_app(
        &self,
        request: &McpAppInvokeRequest,
        attribution: peri_acp_types::mcp_apps::HostInvocationRef,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        self.invoke_app_inner(request, attribution, cancellation)
            .await
    }
}

pub(crate) fn relay_error(kind: McpAppsErrorKind) -> McpAppsRelayError {
    McpAppsRelayError { kind }
}

#[cfg(test)]
#[path = "apps_relay_test.rs"]
mod tests;
