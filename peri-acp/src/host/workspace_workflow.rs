//! Session composition and lifecycle of the Workflow MCP consumers.
use super::SessionEnvironment;
use std::sync::Arc;

impl SessionEnvironment {
    #[cfg(test)]
    pub(crate) fn workflow_projection_client_for_test(
        &self,
    ) -> Option<Arc<crate::host::workflow_projection::Client>> {
        self.workflow_projection
            .lock()
            .as_ref()
            .map(|projection| projection.client.clone())
    }

    /// 装配期将运行时绑定进 builtin `workflow` 槽位；业务数据只经 MCP 消费。
    ///
    /// 返回 `None`：本环境未注入 workflow 实例槽位（该部署形态没有 builtin 实例面）。
    /// 返回 `Some` 时由调用方按 [`peri_mcp_workflow::BindOutcome`] 裁决：`Conflict`
    /// 是装配异常，按会话初始化失败处理（design §6.2），不降级为警告。
    ///
    /// 实例槽位创建独立执行 owner，不注入 Host 的 TaskManager。
    pub(crate) fn bind_workflow_runtime(
        &self,
        runtime: &Arc<peri_mcp_workflow::WorkflowRuntime>,
    ) -> Option<peri_mcp_workflow::BindOutcome> {
        let input = self.workflow_input.as_ref()?;
        Some(input.bind(Arc::clone(runtime)))
    }

    /// 装配期取运行时以绑定消费者令牌；`None` = 未绑定。
    pub(crate) fn workflow_runtime(&self) -> Option<Arc<peri_mcp_workflow::WorkflowRuntime>> {
        self.workflow_input
            .as_ref()
            .and_then(|input| input.runtime())
            .cloned()
    }

    /// 启动宿主侧 workflow 任务消费者（design §5.5；P3 就绪与订阅接线）。
    ///
    /// 前置：槽位已绑定（[`Self::bind_workflow_runtime`] 成功）。消费者在后台
    /// 建立 `workflow://tasks` 订阅并完成基线读取后才置 `consumer_ready`；就绪前
    /// `Workflow` 工具拒绝新启动（就绪屏障），订阅失败回落 not-ready 而非带病放行。
    ///
    /// 返回 `false`：无池 / 无槽位，或初始化完成后确实没有 workflow 实例。
    /// 初始化中的句柄缺席不是能力缺席；失败、超时或缺少运行时绑定必须上报。
    pub(crate) async fn start_workflow_consumer(
        &self,
        executor: Arc<dyn peri_acp_types::workflow::AgentExecutor>,
    ) -> Result<bool, String> {
        let Some(pool) = self.cfg.mcp_pool.clone() else {
            return Ok(false);
        };
        if self.workflow_input.is_none() {
            return Ok(false);
        }
        // MCP 在 activate 后后台初始化，不能用一次瞬时查询永久跳过消费者。
        // 只等待宿主连接状态；任务、就绪和关闭仍经 MCP 协议消费。
        let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(10);
        while !pool.has_instance("workflow") {
            let snapshot = pool.snapshot();
            match snapshot["initPhase"].as_str() {
                Some("ready") => {
                    // 终态快照可能晚于循环条件读取；再次确认，避免漏掉刚注册的实例。
                    if !pool.has_instance("workflow") {
                        return Ok(false);
                    }
                    break;
                }
                Some("failed") => {
                    // 终态且没有 workflow 实例：可能是无关 server 失败或部署未提供该
                    // 实例。会话本身仍可用，不把 Workflow 不可用升级为 session 失败。
                    tracing::warn!(
                        target: "workflow.consumer",
                        "MCP pool initialization failed without a workflow instance; \
                         skipping workflow consumer wiring"
                    );
                    return Ok(false);
                }
                Some("pending" | "initializing") => {}
                _ => {
                    return Err(
                        "workflow consumer setup failed: unknown MCP initialization state".into(),
                    )
                }
            }
            if peri_time::monotonic_now() >= deadline {
                return Err(
                    "workflow consumer setup timed out waiting for MCP instance registration"
                        .into(),
                );
            }
            peri_time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let runtime = self.workflow_runtime().ok_or_else(|| {
            "workflow consumer setup failed: instance exists but runtime slot is not bound"
                .to_string()
        })?;
        // Composition injects only an opaque capability. Runtime data is read over MCP.
        let token = runtime.bind_consumer_token();
        let session_id = self.session_id.clone();
        let inbox = self
            .cfg
            .session_manager
            .session_inbox_for(&session_id)
            .map(|inbox| inbox.handle());
        // A repeated setup must preserve the original pending/accepted receipts.
        // Replacing this client would lose replay authority before source ACK.
        self.workflow_projection.lock().get_or_insert_with(|| {
            crate::host::workflow_projection::WorkflowProjection::start(
                pool.clone(),
                self.task_manager(),
                session_id.clone(),
                token.clone(),
                Arc::new(move |result| {
                    let inbox = inbox
                        .as_ref()
                        .ok_or_else(|| "workflow session inbox unavailable".to_string())?;
                    peri_agent::session::async_router::AsyncRouter::new(inbox.clone())
                        .route_workflow_task_result(result);
                    Ok(())
                }),
            )
        });
        let consumer = crate::host::workflow_consumer::WorkflowTaskConsumer::start(
            pool,
            token,
            executor,
            self.session_id.clone(),
        );
        // 重复调用（异常路径）时旧消费者随赋值 drop → 取消，不残留双消费者。
        *self.workflow_consumer.lock() = Some(consumer);
        Ok(true)
    }

    /// 停止消费者：取消订阅与在飞执行并有界等待收口（会话关闭路径；幂等）。
    pub(super) async fn stop_workflow_consumer(&self) {
        // 锁在本语句内收口（guard 不跨 await），返回后 await 收口。
        let consumer = self.workflow_consumer.lock().take();
        if let Some(consumer) = consumer {
            consumer.stop().await;
        }
    }
}
