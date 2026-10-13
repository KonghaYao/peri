//! `SessionAppsFacade` 的**旧别名落地入口**（`peri/mcp/{open,app,resource}`）。
//!
//! 本文件是 `apps_facade` 的子模块：作为子模块可以访问 `SessionAppsFacade` 的私有
//! 字段（不需要放宽字段可见性），同时把别名专属的适配（token→record 解析、附件身份
//! 定位、服务端补齐 `{epoch, seq}`、Host 归属值）与七个新方法**分文件**承载
//! （`STD-SIZE-001`）。
//!
//! 纪律（§A 运行中裁决 + 裁决 D1）：别名只做字段映射；准入、判决、执行全部走同一
//! facade / ledger / canonical 入口，别名不复制业务规则、不持有状态。

use super::*;

impl SessionAppsFacade {
    // ── 旧别名（`peri/mcp/{open,app,resource}`）的落地入口 ──────────────────────
    //
    // 旧信封没有附件身份 / 纪元 / 序号：附件身份由 `appSessionId`（= record 身份）
    // 合成，纪元取 router 权威值、序号取 ledger 的下一个；其余全部与新方法
    // **同一路径**（别名不复制判决，也不持有状态）。

    /// 旧别名的 record 事实投影（按**附件身份**定位，§A-2）：record 身份取自
    /// router 的权威值（客户端字段只当句柄），不经任何连接级 lease 表。
    pub(crate) async fn alias_record_for_attachment(
        &self,
        attachment_id: &AttachmentId,
    ) -> Result<AliasRecordFacts, peri_acp_types::mcp_apps::McpAppsRelayError> {
        let Some(attachment) = self.router.lookup(attachment_id) else {
            return Err(peri_acp_types::mcp_apps::McpAppsRelayError {
                kind: McpAppsErrorKind::UnknownNamespace,
            });
        };
        self.alias_record(attachment.app_id()).await
    }

    /// 旧别名的 record 事实投影：descriptor + 绑定的 MCP 协议版本。
    ///
    /// 两者都取自服务端事实（`list_apps` 的 descriptor + 由该 descriptor 的
    /// **record 身份**导出的 `binding_protocol_version` 复核），**不**回显客户端
    /// 自述字段。
    pub(crate) async fn alias_record(
        &self,
        app_id: &str,
    ) -> Result<AliasRecordFacts, peri_acp_types::mcp_apps::McpAppsRelayError> {
        let descriptor = self
            .relay
            .list_apps(&self.session_id)
            .await?
            .into_iter()
            .find(|descriptor| descriptor.app_id == app_id)
            .ok_or(peri_acp_types::mcp_apps::McpAppsRelayError {
                kind: McpAppsErrorKind::PolicyDenied,
            })?;
        // 由 record 事实导出的绑定复核（不是客户端字段）：按**记录身份**复核 server
        // 世代与连接；世代不符 ⇒ `StaleServerGeneration`，断连 ⇒ `ServerDisconnected`。
        let mcp_protocol_version = self
            .relay
            .binding_protocol_version(&descriptor.app_id)
            .await?;
        Ok(AliasRecordFacts {
            descriptor,
            mcp_protocol_version,
        })
    }

    /// 旧别名的请求落地：服务端补齐 `{attachment, epoch, seq}` 后走 `apps/request`。
    ///
    /// 返回 `Pending` 表示有界等待内未取得终态（客户端应重试；旧 wire 无同 key
    /// 语义，不得谎报结果）。
    pub(crate) async fn alias_request(
        self: &Arc<Self>,
        conn: &str,
        attachment_id: &AttachmentId,
        operation: AppsOperation,
    ) -> Result<AppsRequestResponse, AcpError> {
        // 旧信封的 `appSessionId` 语义 = `attachment_id`（§A 运行中裁决：旧客户端
        // 视为不透明句柄）；record 身份由 router 的权威值导出，客户端字段不参与。
        let Some(attachment) = self.router.lookup(attachment_id) else {
            return Err(verdict_error(RequestVerdict::UnknownNamespace));
        };
        let attachment_id = attachment.attachment_id().clone();
        let namespace = attachment.namespace().clone();
        let seq = self.ledger.next_seq(&namespace);
        let request = AppsRequestRequest {
            session_id: self.session_id.clone(),
            attachment_id,
            epoch: attachment.epoch(),
            seq,
            operation,
        };
        let response = self.request(conn, request).await?;
        // 旧 wire 是同步语义：`Accepted`（快速返回 Pending）之后在此等终态（有界）；
        // 超时如实返回 `Pending`（不谎报结果，由别名层投影成显式错误）。
        if matches!(response.outcome, Some(AppsRequestOutcome::Pending)) {
            if let Some(slot) = self.ledger.slot(&namespace, seq) {
                let outcome = slot
                    .wait(std::time::Duration::from_millis(REQUEST_WAIT_TIMEOUT_MS))
                    .await;
                return Ok(AppsRequestResponse {
                    outcome: Some(outcome.unwrap_or(AppsRequestOutcome::Pending)),
                    ..response
                });
            }
        }
        Ok(response)
    }
}
