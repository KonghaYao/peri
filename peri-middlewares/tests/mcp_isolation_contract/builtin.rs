//! Production builtin instance isolation contract (I01).

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use peri_acp_types::{
    builtin_mcp::find as find_builtin_instance,
    ports::{McpPoolPort, McpPoolShutdownReport},
};
use peri_mcp_cron::{CronScheduler, CronTrigger};
use peri_middlewares::assembly::{BuiltinInstanceContext, CronInstanceInput};
use peri_middlewares::mcp::{
    ClientStatus, McpClientHandle, McpClientPool, McpInitStatus, McpTaskOwner,
};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::EnvIsolation;

// ─── I01（主 plan §8 第 15 行 / R25）：真实 builtin cron 的隔离 ─────────────────────
//
// 上面四条用例走的是**两台 stdio fixture** + `PERI_MCP_BUILTIN=off`：它们证明不了真实
// builtin 实例的隔离（主 plan §0.1 IF-P3-12 / §8 第 15 行的 F6 口径）。本节的用例走
// **生产装配的完整步骤序列**（pool → `bind_execution_cwd` → 上下文注入 → `run_initialize`），
// 实例是真实 `ServerHandler` + 同进程 duplex transport，且**被注入的状态对象由本
// 测试持有**（cron scheduler），因此「transport / server task / 状态对象
// 是否独立」在公开面上逐条可证伪。

/// `cron` 实例名（与注册表 `BuiltinMcpInstance::name` 逐字一致；本文件不硬编码
/// 工具名，工具面一律从注册表派生）。
const BUILTIN_CRON: &str = "cron";

/// 真实调用的有界等待：必须**成功**，超时即失败。
const LIVE_CALL_BOUND: Duration = Duration::from_secs(30);
/// 已收敛实例上的调用有界等待：必须**失败**，不允许既超时又不失败。
const DEAD_CALL_BOUND: Duration = Duration::from_secs(10);
/// tick **正向**等待：生产 tick 周期是 1s（`mcp::builtin::runtime::BUILTIN_TICK_INTERVAL`，
/// `pub(crate)`，读不到），取 10× 周期。
const TICK_WAIT_BOUND: Duration = Duration::from_secs(10);
/// tick **负向**窗口：3× 周期内一次触发都不出现才可判「tick task 已收敛」。
const TICK_ABSENCE_WINDOW: Duration = Duration::from_secs(3);

/// 真实 builtin 池夹具：已实现的 `web` / `artifact` / `cron` / `workspace`
/// 由**缺省注入**产生，本用例只断言 `cron`（其余实例在场不影响这些断言，
/// 反而是「关闭一个不影响其他实例」的额外见证）。
///
/// 注入的 `scheduler` 由本夹具持有：被注入状态对象的**身份**因此可观察
/// （cron 调用是否落在同一份 scheduler）。
struct BuiltinIsolationFixture {
    _dir: tempfile::TempDir,
    _env: EnvIsolation,
    pool: Arc<McpClientPool>,
    tasks: McpTaskOwner,
    scheduler: Arc<Mutex<CronScheduler>>,
    triggers: mpsc::UnboundedReceiver<CronTrigger>,
}

impl BuiltinIsolationFixture {
    /// 该实例的池条目（不存在即失败）。
    fn handle(&self, instance: &str) -> Arc<McpClientHandle> {
        self.pool.get_client(instance).unwrap_or_else(|| {
            let known: Vec<String> = self
                .pool
                .get_all_clients()
                .iter()
                .map(|handle| handle.name.clone())
                .collect();
            panic!("{instance} 必须已在池中，实际条目: {known:?}")
        })
    }

    /// 该实例已真实连上（未连上时后续断言全部空洞，必须先证伪这一条）。
    fn connected(&self, instance: &str) -> Arc<McpClientHandle> {
        let handle = self.handle(instance);
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "{instance} 必须是 Connected，实际: {:?}",
            handle.status
        );
        assert!(handle.peer.is_some(), "{instance} 必须持有 peer");
        handle
    }

    fn connected_count(&self) -> usize {
        self.pool
            .get_all_clients()
            .iter()
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .count()
    }

    /// 实例是否**仍在服务**：向它发一次必然被拒的 `tools/call`，按错误种类区分。
    ///
    /// - 仍在服务的实例由 handler 用 `invalid_params("unknown tool: ...")` 拒绝
    ///   （`McpError`）⇒ server task 仍在处理请求；
    /// - 已收敛的实例连不上 ⇒ 传输层错误（另一个变体）。
    ///
    /// 探针不依赖任何真实工具名与参数，因此对**不能触发副作用**的实例（如 `artifact`）
    /// 同样适用；两个方向都有判别力（活 ⇒ Err(McpError)，死 ⇒ 非 McpError）。
    async fn serving_probe(handle: &McpClientHandle, bound: Duration) -> Result<(), String> {
        let peer = handle
            .peer
            .clone()
            .ok_or_else(|| format!("{} 没有 peer（未连接）", handle.name))?;
        let request = CallToolRequestParams::new("__iso_liveness_probe__".to_string());
        match tokio::time::timeout(bound, peer.call_tool(request)).await {
            Err(_) => Err(format!(
                "{}::tools/call 在 {bound:?} 内没有返回",
                handle.name
            )),
            Ok(Ok(_)) => Err(format!(
                "{} 上不存在的工具名竟然调用成功（夹具假设不成立）",
                handle.name
            )),
            Ok(Err(rmcp::ServiceError::McpError(error))) => {
                assert!(
                    error.message.contains("unknown tool"),
                    "{} 必须以未知工具拒绝探针，实际: {}",
                    handle.name,
                    error.message
                );
                Ok(())
            }
            Ok(Err(error)) => Err(format!("{} 已不再服务: {error:?}", handle.name)),
        }
    }

    /// 经该句柄自己的 peer 发一次真实 `tools/call`（有界），成功时返回文本结果。
    async fn call(
        &self,
        handle: &McpClientHandle,
        tool: &str,
        arguments: Value,
        bound: Duration,
    ) -> Result<String, String> {
        let peer = handle
            .peer
            .clone()
            .ok_or_else(|| format!("{} 没有 peer（未连接）", handle.name))?;
        let request = CallToolRequestParams::new(tool.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        match tokio::time::timeout(bound, peer.call_tool(request)).await {
            Err(_) => Err(format!("{}::{tool} 在 {bound:?} 内没有返回", handle.name)),
            Ok(Err(error)) => Err(format!("{}::{tool} 调用失败: {error}", handle.name)),
            Ok(Ok(result)) => {
                let text = content_text(&result.content);
                if result.is_error.unwrap_or(false) {
                    Err(format!("{}::{tool} 返回错误结果: {text}", handle.name))
                } else {
                    Ok(text)
                }
            }
        }
    }

    /// `cron_list` 的真实调用（结果文本会带上该任务的 prompt）。
    async fn call_cron_list(&self, handle: &McpClientHandle) -> String {
        self.call(handle, "cron_list", json!({}), LIVE_CALL_BOUND)
            .await
            .unwrap_or_else(|error| panic!("cron 实例必须仍可调用: {error}"))
    }

    /// 注入 scheduler 里的任务数（唯一真值来自**测试持有**的那份 scheduler）。
    fn cron_task_count(&self) -> usize {
        self.scheduler.lock().list_tasks().len()
    }

    fn cron_task_id(&self) -> String {
        let scheduler = self.scheduler.lock();
        scheduler
            .list_tasks()
            .first()
            .map(|task| task.id.clone())
            .unwrap_or_else(|| panic!("注入 scheduler 里必须已有任务"))
    }

    /// 正向 tick 观测：排空历史触发 → 把任务强制到点 → 等一次触发。
    ///
    /// 触发只会由**池侧本代 tick task**（每 1s 一次 `scheduler.lock().tick()`）产生；
    /// 本测试自己从不调 `tick()`，因此收到触发 == 该实例的 tick task 正在跑。
    async fn await_cron_tick(&mut self, label: &str) -> CronTrigger {
        while self.triggers.try_recv().is_ok() {}
        let task_id = self.cron_task_id();
        assert!(
            self.scheduler.lock().force_next_fire_to_past(&task_id),
            "[{label}] 强制到点必须命中已注册任务"
        );
        match tokio::time::timeout(TICK_WAIT_BOUND, self.triggers.recv()).await {
            Ok(Some(trigger)) => trigger,
            Ok(None) => panic!("[{label}] cron 触发通道被关闭"),
            Err(_) => panic!(
                "[{label}] {TICK_WAIT_BOUND:?} 内没有触发：cron 实例的 tick task 不在跑（或已收敛）"
            ),
        }
    }

    /// 负向 tick 观测：排空历史触发 → 强制到点 → `TICK_ABSENCE_WINDOW` 内不得有触发。
    async fn assert_no_cron_tick(&mut self, label: &str) {
        while self.triggers.try_recv().is_ok() {}
        let task_id = self.cron_task_id();
        assert!(
            self.scheduler.lock().force_next_fire_to_past(&task_id),
            "[{label}] 强制到点必须命中已注册任务"
        );
        match tokio::time::timeout(TICK_ABSENCE_WINDOW, self.triggers.recv()).await {
            Err(_) => {}
            Ok(Some(trigger)) => panic!(
                "[{label}] 已收敛的实例仍在被 tick 驱动（task={}）",
                trigger.task_id
            ),
            Ok(None) => {}
        }
    }

    /// 观测 tick 周期的**上界**：连续两次「强制到点 → 等触发」之间的间隔。
    ///
    /// 生产 tick 周期是 `pub(crate)` 常量（集成目标读不到）。负向断言「窗口内没有触发」
    /// 只有在窗口 ≥ 一个周期时才有判别力，因此这里把该前提从假设变成观测：两次相邻
    /// 触发都只能落在 tick 边界上 ⇒ 间隔 ≤ 一个周期。调用方据此断言
    /// `间隔 < TICK_ABSENCE_WINDOW`，负向窗口的有效性就不再依赖外部常量。
    async fn measure_tick_period_upper_bound(&mut self, label: &str) -> Duration {
        self.await_cron_tick(label).await;
        let started = std::time::Instant::now();
        self.await_cron_tick(label).await;
        started.elapsed()
    }
}

/// 注册表声明的原始工具名（断言一律从注册表派生，不硬编码工具名）。
fn declared_original_tools(instance: &str) -> Vec<String> {
    let mut names: Vec<String> = find_builtin_instance(instance)
        .unwrap_or_else(|| panic!("注册表必须含实例 {instance}"))
        .tools
        .iter()
        .map(|tool| tool.original_name.to_string())
        .collect();
    names.sort();
    names
}

/// 句柄上 `tools/list` 的工具名（排序后比较，顺序由 crate 内用例负责）。
fn handle_tool_names(handle: &McpClientHandle) -> Vec<String> {
    let mut names: Vec<String> = handle
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    names
}

/// `CallToolResponse` 的文本投影（内置 handler 的成功结果都是单条文本）。
fn content_text(contents: &[ContentBlock]) -> String {
    contents
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 生产装配步骤序列 + 缺省 builtin 注入 + 测试持有的状态对象。
async fn builtin_isolation_fixture() -> BuiltinIsolationFixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let claude_home = dir.path().join("claude");
    let cwd = dir.path().join("project");
    for path in [&home, &claude_home, &cwd] {
        std::fs::create_dir_all(path).unwrap();
    }
    // 缺省注入（`PERI_MCP_BUILTIN` 缺失 ⇒ 全部已实现实例）；HOME 重定向到临时目录，
    // 因此读不到开发者本机的 `~/.claude.json` / `~/.peri/settings.json`。
    let env = EnvIsolation::set_with_builtin_env(&home, None);
    // 不写 `{cwd}/.mcp.json`：本夹具的 server 集合完全由 builtin 默认层注入产生。
    assert!(
        !cwd.join(".mcp.json").exists(),
        "本夹具不得有项目级 MCP 配置"
    );

    let (cron_trigger_tx, triggers) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(cron_trigger_tx)));
    let context = BuiltinInstanceContext::new(cwd.to_string_lossy().into_owned()).with_cron(
        CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            // `tick_enabled = true`（TUI 投影）：池的唯一 spawn 点会为本代 cron 挂 tick，
            // 使「实例的 server task / tick task 是否独立」成为可观察事实。
            tick_enabled: true,
        },
    );

    let (tasks, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    // 与生产装配同一步骤（`host/assemble.rs::pending_mcp_pool`）：先绑定 host cwd，
    // 再注入上下文（A33：注入必须早于 `run_initialize`）。
    pool.bind_execution_cwd(&cwd)
        .expect("夹具第一次绑定 execution cwd");
    pool.set_builtin_instance_context(Arc::new(context))
        .expect("首次注入必须成功（夹具不会二次注入）");

    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(pool.clone(), &cwd, &claude_home, status_tx, None).await;

    BuiltinIsolationFixture {
        _dir: dir,
        _env: env,
        pool,
        tasks,
        scheduler,
        triggers,
    }
}

/// **真实 builtin `cron` 的 transport / server task / 状态对象独立于其它实例**，
/// 且实例级 close 只收敛被关的那一个、pool 级 shutdown 才同时收敛全部。
///
/// 与上面四条 stdio 用例的差别（主 plan §8 第 15 行的 F6 口径）：那些用例
/// `PERI_MCP_BUILTIN=off` + 两台独立 stdio 子进程，**不能**作为 builtin 隔离证据；
/// 本用例的结论只来自真实 builtin 实例（同进程 duplex + 真实 `ServerHandler`），
/// 观察面全部是 `peri-middlewares` 的 `pub` API。
///
/// 观察面与判据（每条都可证伪）：
///
/// 1. **transport / 句柄 / 工具面**：句柄的工具面等于注册表为该实例声明的集合；实例
///    上报 `transport = "builtin"`；调用经自己的 peer 落回自己的 handler（cron 调用只改
///    注入 scheduler）。
/// 2. **server task / tick task**：cron 代的 tick 触发只可能来自池侧本代 tick task
///    （测试从不自己 tick）。
/// 3. **close 一个不动另一个**：`set_disabled` / `remove_server` 后，被关实例的旧 peer
///    有界失败、cron 的 tick 收敛；对侧句柄仍是**同一份 `Arc`**、仍 `Connected`、真实调用
///    仍成功。
/// 4. **reconnect 代数不串台**：被重连实例换新句柄（旧代 peer 失效）；
///    注入的状态对象跨代保持（cron 任务数不变）。
/// 5. **pool 级 shutdown 才同时收敛**：实例转 `Disconnected` + `peer = None`，
///    旧 peer 有界失败、tick 收敛，且 shutdown 报告收口为 `Complete`（计数与关闭前
///    已连接数一致）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instances_have_independent_transport_task_and_state() {
    let mut fixture = builtin_isolation_fixture().await;

    // ── 步骤 0：装配前提（实例真连上，否则下面的断言面是空的）──────────────────
    assert_eq!(
        fixture.pool.snapshot()["initPhase"],
        json!("ready"),
        "builtin 池必须收口为 ready（失败 ⇒ cron 未连上，断言面空洞）"
    );
    let infos = fixture.pool.all_server_infos();
    let info = infos
        .iter()
        .find(|info| info.name == BUILTIN_CRON)
        .unwrap_or_else(|| panic!("宿主投影必须含 {BUILTIN_CRON}"));
    assert_eq!(
        info.transport_type, "builtin",
        "{BUILTIN_CRON} 必须是 builtin 分类（同进程 transport），实际: {}",
        info.transport_type
    );
    let cron = fixture.connected(BUILTIN_CRON);
    // 正向 tick 观测计数（证据行用）。
    let mut tick_fires = 0usize;

    // ── 步骤 1：transport / 句柄 / 工具面 ───────────────────────────────────────
    assert_eq!(cron.name.as_str(), BUILTIN_CRON);
    assert_eq!(
        handle_tool_names(&cron),
        declared_original_tools(BUILTIN_CRON),
        "{BUILTIN_CRON} 的工具面必须来自自己那次握手，且等于注册表声明"
    );

    // cron 侧真实调用：结果必须落回**测试注入**的那一份 scheduler。
    let registered = fixture
        .call(
            &cron,
            "cron_register",
            json!({ "expression": "0 3 * * *", "prompt": "iso-cron-probe" }),
            LIVE_CALL_BOUND,
        )
        .await
        .unwrap_or_else(|error| panic!("cron 实例必须可调用: {error}"));
    assert!(
        registered.contains("iso-cron-probe"),
        "cron_register 的返回必须来自本实例的工具面: {registered}"
    );
    assert_eq!(
        fixture.cron_task_count(),
        1,
        "任务必须落在被注入的那**一份** scheduler 上（0 = 不是同一份；>1 = 有两份状态）"
    );

    // cron 代的 tick task 正在跑（触发只可能来自池侧 tick）。
    let trigger = fixture
        .await_cron_tick("连接后 cron tick 必须由池侧驱动")
        .await;
    tick_fires += 1;
    assert_eq!(
        trigger.prompt, "iso-cron-probe",
        "触发必须属于本实例注册的任务"
    );
    // 负向断言的窗口必须 ≥ 一个 tick 周期：用**观测到的周期上界**自证，不依赖外部常量。
    let tick_period_upper = fixture
        .measure_tick_period_upper_bound("周期上界观测：相邻两次触发")
        .await;
    tick_fires += 2;
    assert!(
        tick_period_upper < TICK_ABSENCE_WINDOW,
        "负向窗口 {TICK_ABSENCE_WINDOW:?} 必须覆盖一个 tick 周期，实际观测上界 {tick_period_upper:?}"
    );

    // ── 步骤 2：close `cron` —— 自身 server task / tick task 收敛，对侧不动 ───────
    let artifact_before = fixture.connected("artifact");
    let stale_cron = fixture.connected(BUILTIN_CRON);
    fixture.pool.set_disabled(BUILTIN_CRON).await;
    let cron_disabled = fixture.handle(BUILTIN_CRON);
    assert!(
        matches!(cron_disabled.status, ClientStatus::Disabled),
        "set_disabled 后必须是 Disabled，实际: {:?}",
        cron_disabled.status
    );
    assert!(cron_disabled.peer.is_none(), "被关闭实例不得保留 peer");
    // 该实例的 server task 已收敛：它自己的旧 peer 有界失败。
    assert!(
        fixture
            .call(&stale_cron, "cron_list", json!({}), DEAD_CALL_BOUND)
            .await
            .is_err(),
        "已关闭的 cron 实例上，旧 peer 的调用必须失败"
    );
    // tick task 必须随实例一起收敛（否则它仍在驱动已关闭实例的状态对象）。
    fixture
        .assert_no_cron_tick("cron 关闭后 tick task 必须已收敛")
        .await;
    // 对侧不受影响：同一份 Arc、仍 Connected。
    let artifact_after = fixture.connected("artifact");
    assert!(
        Arc::ptr_eq(&artifact_before, &artifact_after),
        "cron 的关闭替换了 artifact 的句柄"
    );

    // ── 步骤 3：reconnect `cron` —— 状态对象跨代保持、tick 重新挂上 ──────────────
    fixture
        .pool
        .reconnect(BUILTIN_CRON, None)
        .await
        .expect("cron 重连必须成功");
    let cron_new = fixture.connected(BUILTIN_CRON);
    assert!(
        !Arc::ptr_eq(&cron_new, &stale_cron),
        "重连必须换新代句柄（旧代 `Arc` 不得被复用）"
    );
    assert_eq!(
        handle_tool_names(&cron_new),
        declared_original_tools(BUILTIN_CRON),
        "新代实例的工具面必须重新来自自己那次握手"
    );
    assert_eq!(
        fixture.cron_task_count(),
        1,
        "重连不得复制或丢失 cron 侧状态（同一份注入 scheduler）"
    );
    assert!(
        fixture
            .call_cron_list(&cron_new)
            .await
            .contains("iso-cron-probe"),
        "新代 cron 必须读到同一份状态对象里的任务"
    );
    fixture
        .await_cron_tick("cron 重连后新代的 tick task 必须重新挂上")
        .await;
    tick_fires += 1;
    let artifact_untouched = fixture.connected("artifact");
    assert!(
        Arc::ptr_eq(&artifact_before, &artifact_untouched),
        "cron 的重连替换了 artifact 的句柄（代数串台）"
    );

    // ── 步骤 4：终态关闭面（`remove_server`，连带删配置）不动在场实例 ─────────────
    //
    // 关闭面有两个公开入口：`set_disabled`（步骤 2，保留配置与面板条目）与
    // `remove_server`（终态关闭，连带删配置）。两者共用同一份物理收敛
    // （`close_builtin_task`），但**入口语义不同**，因此补一条真实 builtin 实例的
    // `remove_server` 证据。被关的是**在场的第三个** builtin 实例，cron 留到步骤 5
    // 才能断言「pool 级 shutdown 才同时收敛全部」。
    let artifact = "artifact";
    let cron_alive = fixture.connected(BUILTIN_CRON);
    // 正向对照：被关**之前**它确实在服务（否则下面的失败无法归因于关闭）。
    assert!(
        BuiltinIsolationFixture::serving_probe(&artifact_before, DEAD_CALL_BOUND)
            .await
            .is_ok(),
        "{artifact} 在 remove_server 之前必须在服务"
    );
    fixture.pool.remove_server(artifact).await;
    assert!(
        fixture.pool.get_client(artifact).is_none(),
        "remove_server 必须移除该实例的池条目（连带配置）"
    );
    assert!(
        BuiltinIsolationFixture::serving_probe(&artifact_before, DEAD_CALL_BOUND)
            .await
            .is_err(),
        "remove_server 后旧 peer 必须失效（server task 已收敛）"
    );
    let cron_after_removal = fixture.connected(BUILTIN_CRON);
    assert!(
        Arc::ptr_eq(&cron_alive, &cron_after_removal),
        "cron 的句柄被 {artifact} 的 remove_server 替换了"
    );
    assert!(
        fixture
            .call_cron_list(&cron_alive)
            .await
            .contains("iso-cron-probe"),
        "remove_server 第三方实例后 cron 必须仍可真实调用"
    );

    // ── 步骤 5：pool 级 shutdown 才**同时**收敛全部 ─────────────────────────────
    let services_before = fixture.connected_count();
    let cron_before_shutdown = fixture.connected(BUILTIN_CRON);
    fixture.tasks.begin_shutdown();
    let _ = fixture.tasks.shutdown().await;
    let report = fixture.pool.shutdown().await;
    match report {
        McpPoolShutdownReport::Complete {
            settled_services,
            failed_services,
        } => {
            assert_eq!(failed_services, 0, "shutdown 不得留下失败的服务");
            assert_eq!(
                settled_services, services_before,
                "pool 级 shutdown 必须收敛**全部**已连接实例"
            );
        },
        McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        } => panic!(
            "pool 级 shutdown 未收口: settled={settled_services} unfinished={unfinished_services} failed={failed_services}"
        ),
    }
    let cron_handle = fixture.handle(BUILTIN_CRON);
    assert!(
        matches!(cron_handle.status, ClientStatus::Disconnected),
        "{BUILTIN_CRON} 必须在 pool shutdown 后转 Disconnected，实际: {:?}",
        cron_handle.status
    );
    assert!(cron_handle.peer.is_none(), "{BUILTIN_CRON} 不得保留 peer");
    assert!(
        fixture
            .call(
                &cron_before_shutdown,
                "cron_list",
                json!({}),
                DEAD_CALL_BOUND
            )
            .await
            .is_err(),
        "{BUILTIN_CRON} 的 server task 必须在 pool shutdown 后结束（旧 peer 调用必须失败）"
    );
    fixture
        .assert_no_cron_tick("pool shutdown 后 cron 的 tick task 必须已收敛")
        .await;

    // 现场证据行（`--nocapture` 可见；数值供验收台账记录）。
    println!(
        "[I01 builtin isolation] handles: cron | transport=builtin | \
         cron_tasks={} tick_fires={} tick_period<={:?} \
         tick_absent=2 | close/reconnect=one-sided | shutdown=Complete(settled={services_before}, failed=0)",
        fixture.cron_task_count(),
        tick_fires,
        tick_period_upper,
    );
}
