use super::Arc;
use super::BaseTool;
use super::BuiltinInstanceContext;
use super::CronInstanceInput;
use super::CronScheduler;
use super::Duration;
use super::HashSet;
use super::LoaderEnvGuard;
use super::McpClientPool;
use super::McpInitStatus;
use super::McpMiddleware;
use super::McpToolBridge;
use super::WorkflowInstanceInput;
use peri_agent::middleware::Middleware as _;

// V-02 / WP-MW（wave 3）：关闭四维矩阵、四类关闭来源分列、tick 与取消收敛
// （主 plan §8 第 10、25 行；sub-plan-v §5 真值表与生命周期表）
//
// 证据边界（不得读成本节 = 全部链路已端到端验证）：
//
// **A. 真宿主装配形态**（[`Wave3Fixture`]）：真 loader（含 step 6.5 默认层）→ 真
// transport（`spawn_builtin_transport`）→ 真 handler（web / artifact / cron / workspace /
// workflow）→
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
    /// `{cwd}/.mcp.json` 原文（None = 不写文件 ⇒ 默认层注入）。
    project_mcp_json: Option<&'static str>,
    /// `PERI_MCP_BUILTIN` 取值（None = 移除该变量 = 缺省语义）。
    builtin_env: Option<&'static str>,
    /// 会话冻结关闭集（迁移期实例名；v5 单出口按能力身份消费）。
    closed_instances: Vec<&'static str>,
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
        // 本夹具的既有断言按 v4 六实例逐实例观测；缺省 token 固定为回滚路径，
        // v5 默认路径由 `builtin_v5_single_exit_*` 用例显式覆盖。
        let env =
            LoaderEnvGuard::redirect_with_builtin_env(&home, spec.builtin_env.or(Some("legacy")));
        let cwd = project.to_string_lossy().to_string();

        let pool = Arc::new(McpClientPool::new_pending());
        super::bind_fixture_session(&pool);
        let (cron_trigger_tx, triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(cron_trigger_tx)));
        // A33：实例上下文由宿主装配在 `run_initialize` **之前**注入（本夹具扮演宿主）。
        // 与 `StartupFixture` 同形：cron 输入 + `workflow` 空槽（P3–P5 双轨期与会话装配
        // 同构——运行时由会话旧链在 `session/new` 之后绑定，槽位未绑定不影响实例连接）。
        pool.set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new(cwd)
                .with_cron(CronInstanceInput {
                    scheduler: Arc::clone(&scheduler),
                    tick_enabled: spec.tick_enabled,
                })
                .with_workflow(WorkflowInstanceInput::slot())
                .with_closed(
                    spec.closed_instances
                        .iter()
                        .map(|instance| (*instance).to_string())
                        .collect(),
                ),
        ))
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
        crate::mcp::tool_bridge::build_typed_tool_bridges(&self.pool)
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
        .with_builtin_closures(super::closed_instances(&disabled))
        .collect_tools("/tmp/mw-projection")
}

/// 投影里某实例对模型可见的工具名；宿主工具仍可在 MCP tools/list 中发现。
fn mw_instance_tools(tools: &[Box<dyn BaseTool>], instance: &str) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| tool.builtin_mcp_instance() == Some(instance) && tool.visible_to_model())
        .map(|tool| tool.name().to_string())
        .collect()
}

/// 注册表声明的某实例工具数（期望的可见性计数一律**从注册表派生**，不硬编码条数）：
/// 注册表增删工具时相关用例自动跟随。
fn mw_declared_tool_count(instance: &str) -> usize {
    super::declared_effective_names(super::find(instance).expect("实例必须已注册")).len()
}

#[path = "builtin_runtime_policy_test.rs"]
mod policy;

#[path = "builtin_runtime_closure_test.rs"]
mod closure;

// ─── v5 单出口默认路径（宿主装配 + 真实 transport）─────────────────────────────

/// 默认装配只产生一个 builtin server `peri`：能力工具 / 资源 / 宿主调用都经出口聚合。
#[tokio::test]
async fn v5_default_injects_single_exit_and_aggregates_capabilities() {
    let fixture = Wave3Fixture::start(Wave3Spec {
        builtin_env: Some("v5"),
        ..Wave3Spec::default()
    })
    .await;
    let pool = Arc::clone(&fixture.pool);

    assert_eq!(
        pool.builtin_task_count(),
        1,
        "v5 默认路径只允许一个 builtin transport / server task"
    );
    let handle = pool.client("peri").expect("单出口 peri 必须已连接");
    assert!(matches!(handle.status, super::ClientStatus::Connected));
    for legacy in super::BUILTIN_MCP_INSTANCES.iter() {
        assert!(
            pool.client(legacy.name).is_none(),
            "{} 不得再作为独立 builtin server 出现",
            legacy.name
        );
    }

    let served: HashSet<String> = pool
        .tools_for_server("peri")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    for capability in peri_acp_types::builtin_mcp::BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools.iter().filter(|tool| tool.model_visible) {
            assert!(
                served.contains(tool.original_name),
                "{} 的 wire 工具 {} 必须由单出口列出",
                capability.name,
                tool.original_name
            );
        }
    }
    assert!(served.contains("Bash"), "workspace direct 工具保持原名");
    assert!(
        served.contains("cron_list"),
        "deferred 工具在 wire 目录保留原始名"
    );
    let bridges = crate::mcp::tool_bridge::build_typed_tool_bridges(&pool);
    assert!(
        bridges.iter().any(|bridge| {
            bridge.mcp_server_name() == Some("peri") && bridge.name() == "mcp__peri__cron_list"
        }),
        "deferred 工具的模型面名字必须由工具桥生成"
    );

    let resources: HashSet<String> = pool
        .resources_for_server("peri")
        .into_iter()
        .map(|resource| resource.uri.to_string())
        .collect();
    for uri in ["workspace://git/ref", "cron://jobs", "workflow://runs"] {
        assert!(resources.contains(uri), "资源 {uri} 必须由单出口聚合");
    }

    // 宿主按 (capability, original_name) 消费：出口内翻译到模型面名字后路由。
    pool.call_instance_tool("cron", "cron_list", super::json!({}), None)
        .await
        .expect("cron_list 必须经单出口执行");
    // 夹具只给 workflow 空槽（与会话装配同形）：资源读取必须到达 workflow 能力，
    // 并以「运行时尚未绑定」拒绝，而不是 -32601 / 未知资源。
    let error = pool
        .read_instance_resource("workflow", "workflow://runs")
        .await
        .expect_err("未绑定运行时必须拒绝");
    assert!(
        error.contains("workflow runtime is not bound"),
        "资源必须路由到 workflow 能力: {error}"
    );

    let bash = bridges
        .iter()
        .find(|bridge| {
            bridge.mcp_server_name() == Some("peri") && bridge.original_tool_name() == "Bash"
        })
        .expect("Bash bridge 必须存在");
    assert!(bash.is_direct(), "workspace direct 工具必须保持 direct");
    let cron = bridges
        .iter()
        .find(|bridge| {
            bridge.mcp_server_name() == Some("peri") && bridge.original_tool_name() == "cron_list"
        })
        .expect("cron_list bridge 必须存在");
    assert!(!cron.is_direct(), "deferred 工具不得提升为 direct");

    fixture.shutdown().await;
}

/// 关闭一个能力时，单出口的工具、direct 提升、必需工具与宿主调用同步消失。
#[tokio::test]
async fn v5_capability_closure_hides_all_faces() {
    let fixture = Wave3Fixture::start(Wave3Spec {
        builtin_env: Some("v5"),
        closed_instances: vec!["web"],
        ..Wave3Spec::default()
    })
    .await;
    let pool = Arc::clone(&fixture.pool);

    let served: HashSet<String> = pool
        .tools_for_server("peri")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(
        !served.contains("WebSearch"),
        "关闭 web 后不得列出 WebSearch"
    );
    assert!(!served.contains("WebFetch"), "关闭 web 后不得列出 WebFetch");
    assert!(served.contains("Bash"), "其他能力不受影响");

    let closed: HashSet<String> = HashSet::from(["web".to_string()]);
    let requirement = pool
        .system_requirements()
        .into_iter()
        .find(|requirement| requirement.server == "peri")
        .expect("peri 必须进入 system 依赖");
    let visible_required = crate::mcp::builtin::visible_required_tools(
        "peri",
        &requirement.required_tools,
        &closed
            .iter()
            .map(|name| name.to_string())
            .collect::<std::collections::BTreeSet<_>>(),
    );
    assert!(!visible_required.iter().any(|tool| tool == "WebSearch"));
    assert!(!visible_required.iter().any(|tool| tool == "WebFetch"));
    assert!(visible_required.iter().any(|tool| tool == "Bash"));

    let bridges = crate::mcp::tool_bridge::build_typed_tool_bridges(&pool);
    assert!(
        !bridges.iter().any(|bridge| {
            bridge.mcp_server_name() == Some("peri") && bridge.original_tool_name() == "WebSearch"
        }),
        "关闭能力的工具不得进入 bridge 集合"
    );

    assert!(
        pool.call_instance_tool("web", "WebSearch", super::json!({"query":"x"}), None)
            .await
            .is_err(),
        "关闭能力必须由服务端拒绝执行，而不是只隐藏目录"
    );

    fixture.shutdown().await;
}

/// 关闭 workspace 时默认订阅同步消失（git ref 不再建立）。
#[tokio::test]
async fn v5_workspace_closure_drops_default_subscriptions() {
    let fixture = Wave3Fixture::start(Wave3Spec {
        builtin_env: Some("v5"),
        closed_instances: vec!["workspace"],
        ..Wave3Spec::default()
    })
    .await;
    let pool = Arc::clone(&fixture.pool);
    let subscriptions = peri_acp_types::plugin::McpSubscriptionsConfig {
        resources: vec!["workspace://git/ref".to_string()],
        tools_list_changed: false,
        prompts_list_changed: false,
        resources_list_changed: false,
    };
    assert!(
        pool.filter_subscriptions("peri", &subscriptions).is_none(),
        "workspace 关闭时不得再订阅 workspace 资源"
    );
    fixture.shutdown().await;
}

/// V5-C 关闭矩阵：六种能力逐一关闭后，wire 工具、required 工具、typed bridge 与
/// 运行时调用四个面同时消失，且其他能力保持可见。
///
/// 宿主任务面（cron tick / monitor 轮询 / workflow 消费者）由
/// `builtin_runtime_policy_test` 的生命周期用例覆盖；本用例锁定同一关闭集在
/// 目录与执行两端的投影一致性。
#[tokio::test]
async fn v5_closure_matrix_covers_every_capability() {
    for capability in peri_acp_types::builtin_mcp::BUILTIN_V5_CAPABILITIES {
        let closed_tools: HashSet<&str> = capability
            .tools
            .iter()
            .filter(|tool| tool.advertised && tool.model_visible)
            .map(|tool| tool.original_name)
            .collect();
        assert!(
            !closed_tools.is_empty(),
            "{} 必须有可广告工具",
            capability.name
        );

        let fixture = Wave3Fixture::start(Wave3Spec {
            builtin_env: Some("v5"),
            closed_instances: vec![capability.legacy_instance],
            ..Wave3Spec::default()
        })
        .await;
        let pool = Arc::clone(&fixture.pool);

        let served: HashSet<String> = pool
            .tools_for_server("peri")
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for tool in capability.tools.iter().filter(|tool| tool.advertised) {
            assert!(
                !served.contains(tool.original_name),
                "关闭 {} 后不得列出 {}",
                capability.name,
                tool.original_name
            );
            assert!(
                !served.contains(tool.model_name),
                "关闭 {} 后不得列出模型面名 {}",
                capability.name,
                tool.model_name
            );
        }
        let other_visible = peri_acp_types::builtin_mcp::BUILTIN_V5_CAPABILITIES
            .iter()
            .filter(|other| other.name != capability.name)
            .flat_map(|other| other.tools.iter())
            .filter(|tool| tool.advertised && tool.model_visible)
            .any(|tool| served.contains(tool.original_name) || served.contains(tool.model_name));
        assert!(
            other_visible,
            "关闭 {} 不得连带关闭其他能力",
            capability.name
        );

        let closed: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::from([capability.legacy_instance.to_string()]);
        if let Some(requirement) = pool
            .system_requirements()
            .into_iter()
            .find(|requirement| requirement.server == "peri")
        {
            let visible = crate::mcp::builtin::visible_required_tools(
                "peri",
                &requirement.required_tools,
                &closed,
            );
            for tool in capability.tools.iter().filter(|tool| tool.direct) {
                assert!(
                    !visible.iter().any(|name| name == tool.original_name),
                    "关闭 {} 后必需工具不得保留 {}",
                    capability.name,
                    tool.original_name
                );
            }
        }

        let bridges = crate::mcp::tool_bridge::build_typed_tool_bridges(&pool);
        assert!(
            !bridges.iter().any(|bridge| {
                bridge.mcp_server_name() == Some("peri")
                    && closed_tools.contains(bridge.original_tool_name())
            }),
            "关闭 {} 后其工具不得进入 typed bridge 集合",
            capability.name
        );

        for tool in capability.tools.iter().filter(|tool| tool.advertised) {
            assert!(
                pool.call_instance_tool(
                    capability.legacy_instance,
                    tool.original_name,
                    super::json!({}),
                    None,
                )
                .await
                .is_err(),
                "关闭 {} 的工具 {} 必须由服务端拒绝执行",
                capability.name,
                tool.original_name
            );
        }

        fixture.shutdown().await;
    }
}
