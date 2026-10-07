use super::*;

// V-02 / WP-MW（wave 3）：关闭四维矩阵、四类关闭来源分列、tick 与取消收敛
// （主 plan §8 第 10、25 行；sub-plan-v §5 真值表与生命周期表）
//
// 证据边界（不得读成本节 = 全部链路已端到端验证）：
//
// **A. 真宿主装配形态**（[`Wave3Fixture`]）：真 loader（含 step 6.5 默认层）→ 真
// transport（`spawn_builtin_transport`）→ 真 handler（web / artifact / cron / workspace）→
// 真实握手。与既有 [`StartupFixture`] 的差别**只有两处**：tick 可开（本波需要「1 driver」
// 与「物理关闭后 tick 静默」两个观测面）、可写项目级 `.mcp.json` 与 `PERI_MCP_BUILTIN`
// （关闭来源分列需要）。既有夹具与其全部断言逐位不变。
//
// **C. tick / 物理生命周期维**读既有观测面：`BuiltinInstanceSupervisor::tick_is_finished`、
// `BuiltinInstanceSupervisor::close` 的 `TickCloseOutcome` / `BuiltinServerExit`、
// pool 的 `builtin_task_count` / `builtin_tick_is_finished`（后两者是既有 `#[cfg(test)]`
// 观测面）。超时的真实执行验证由 workspace_recovery_test 承担。

use peri_acp_types::cron::CronTrigger;

/// 本节全部等待的**显式上界**（同进程链路，远小于各内层超时）。
const MW_BOUND: Duration = Duration::from_secs(5);
/// 静默/驱动窗口：必须 **> 2× `BUILTIN_TICK_INTERVAL`**（1s），故取 2400ms。
const MW_TICK_WINDOW: Duration = Duration::from_millis(2400);

// ─── 夹具：真宿主装配形态（可配 tick / 项目 .mcp.json / 注入开关）─────────────────

/// [`Wave3Fixture`] 的装配规格。默认（`Default`）= 与 [`StartupFixture`] 同形（tick 关、
/// 无项目配置、缺省注入全部实例）。
#[derive(Default)]
struct Wave3Spec {
    /// 宿主 `drive_cron_tick` 投影：true ⇒ pool 的唯一 spawn 点为 cron 代挂 tick。
    tick_enabled: bool,
    /// `{cwd}/.mcp.json` 原文（None = 不写文件 ⇒ 四实例全部由默认层注入）。
    project_mcp_json: Option<&'static str>,
    /// `PERI_MCP_BUILTIN` 取值（None = 移除该变量 = 缺省语义 = 注入全部）。
    builtin_env: Option<&'static str>,
}

/// 真宿主装配形态的 pool + cron 驱动观测面。
struct Wave3Fixture {
    _fixture: tempfile::TempDir,
    _env: LoaderEnvGuard,
    pool: Arc<McpClientPool>,
    scheduler: Arc<parking_lot::Mutex<CronScheduler>>,
    triggers: tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
}

impl Wave3Fixture {
    async fn start(spec: Wave3Spec) -> Self {
        let fixture = tempfile::tempdir().expect("tempdir");
        let home = fixture.path().join("home");
        let project = fixture.path().join("project");
        std::fs::create_dir_all(&home).expect("home 可创建");
        std::fs::create_dir_all(&project).expect("project 可创建");
        if let Some(body) = spec.project_mcp_json {
            std::fs::write(project.join(".mcp.json"), body).expect("项目级 .mcp.json 可写");
        }
        let env = LoaderEnvGuard::redirect_with_builtin_env(&home, spec.builtin_env);
        let cwd = project.to_string_lossy().to_string();

        let pool = Arc::new(McpClientPool::new_pending());
        bind_fixture_session(&pool);
        let (cron_trigger_tx, triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(cron_trigger_tx)));
        // A33：实例上下文由宿主装配在 `run_initialize` **之前**注入（本夹具扮演宿主）。
        pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(cwd).with_cron(
            CronInstanceInput {
                scheduler: Arc::clone(&scheduler),
                tick_enabled: spec.tick_enabled,
            },
        )))
        .expect("夹具首次注入上下文必须成功");
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::run_initialize(pool.clone(), &project, &home, status_tx, None).await;

        Self {
            _fixture: fixture,
            _env: env,
            pool,
            scheduler,
            triggers,
        }
    }

    /// 链工具集合（可观察能力面）：`McpMiddleware::collect_tools` 的产出。
    fn projections(&self, disabled: &[&str]) -> Vec<Box<dyn BaseTool>> {
        mw_projection_tools(&self.pool, disabled)
    }

    /// 在册的 cron 任务数（组合根 scheduler 仍可用的正向证据）。
    fn cron_task_count(&self) -> usize {
        self.scheduler.lock().list_tasks().len()
    }

    /// 组合根 scheduler 上注册一个任务并返回 task id。
    fn register_cron_task(&self, expression: &str, prompt: &str) -> String {
        self.scheduler
            .lock()
            .register(expression, prompt)
            .expect("组合根 scheduler 注册必须成功")
    }

    /// 到期后**有界**等一次触发：`> 2× tick 周期` 内必须恰好收到一条（tick 仍 1 driver）。
    async fn await_armed_trigger(&mut self, task_id: &str, prompt: &str) {
        assert!(
            self.scheduler.lock().force_next_fire_to_past(task_id),
            "任务必须仍在册（scheduler 未被关闭）"
        );
        let fired = tokio::time::timeout(MW_TICK_WINDOW, self.triggers.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("[{task_id}] > 2× tick 周期内没有任何触发：tick 驱动已不在运行")
            })
            .expect("观测通道必须存活");
        assert_eq!(fired.task_id, task_id, "触发必须来自被武装的任务");
        assert_eq!(fired.prompt, prompt, "触发必须携带注册时的 prompt");
        assert!(
            self.triggers.try_recv().is_err(),
            "一次武装只允许一条触发（不得有第二个驱动重复送出）"
        );
    }

    /// 反向断言：**已停**的一代不再驱动 scheduler（窗口到期即证据，不用 sleep 作结论）。
    async fn assert_armed_trigger_absent(&mut self, task_id: &str) {
        while self.triggers.try_recv().is_ok() {}
        assert!(
            self.scheduler.lock().force_next_fire_to_past(task_id),
            "反向断言前提：任务必须仍在册"
        );
        let unexpected = tokio::time::timeout(MW_TICK_WINDOW, self.triggers.recv()).await;
        assert!(
            unexpected.is_err(),
            "已关闭的一代不得再驱动 scheduler，实际收到: {:?}",
            unexpected.map(|received| received.map(|trigger| trigger.task_id))
        );
    }

    /// 池中某实例的类型化 bridge（生产构造：声明 direct 生效）。
    fn typed_bridge(&self, effective_name: &str) -> McpToolBridge {
        build_typed_tool_bridges(&self.pool)
            .into_iter()
            .find(|bridge| bridge.name() == effective_name)
            .unwrap_or_else(|| panic!("必须存在 {effective_name} 的 typed bridge"))
    }

    /// 夹具收尾：pool 关闭后 builtin task 表必须已排空（无 orphan）。
    async fn shutdown(self) {
        self.pool.begin_shutdown();
        let report = self.pool.shutdown().await;
        assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
        assert_eq!(
            self.pool.builtin_task_count(),
            0,
            "pool 关闭后不得残留 builtin server task"
        );
    }
}

/// 链工具集合（可观察能力面，**带 direct 标志**）：`McpMiddleware::collect_tools` 的产出。
///
/// 与既有 [`collected_tool_names`] 同一装配方式（`with_builtin_closures` 注入关闭集）；
/// 多返回 `is_direct()`，使「cron 一律 deferred」与「出现/消失」在同一次收集中可断。
fn mw_projection_tools(pool: &Arc<McpClientPool>, disabled: &[&str]) -> Vec<Box<dyn BaseTool>> {
    let disabled: HashSet<String> = disabled.iter().map(|name| name.to_string()).collect();
    McpMiddleware::new(Arc::clone(pool))
        .with_tool_pool(Arc::clone(pool))
        .with_builtin_closures(closed_instances(&disabled))
        .collect_tools("/tmp/mw-projection")
}

/// 投影里某实例前缀（`mcp__{instance}__`）的工具名。
fn mw_instance_tools(tools: &[Box<dyn BaseTool>], instance: &str) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| tool.builtin_mcp_instance() == Some(instance))
        .map(|tool| tool.name().to_string())
        .collect()
}

/// 注册表声明的某实例工具数（期望的可见性计数一律**从注册表派生**，不硬编码条数）：
/// 注册表增删工具时相关用例自动跟随。
fn mw_declared_tool_count(instance: &str) -> usize {
    declared_effective_names(find(instance).expect("实例必须已注册")).len()
}

#[path = "builtin_runtime_policy_test.rs"]
mod policy;

#[path = "builtin_runtime_closure_test.rs"]
mod closure;
