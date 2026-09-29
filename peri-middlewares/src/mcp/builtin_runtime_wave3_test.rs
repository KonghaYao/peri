use super::*;

// V-02 / WP-MW（wave 3）：关闭五维 8 格矩阵、四类关闭来源分列、builtin LSP 外层超时
// 与取消收敛（主 plan §8 第 10、25 行；sub-plan-v §5 真值表与生命周期表）
//
// 证据边界（不得读成本节 = 全部链路已端到端验证）：
//
// **A. 真宿主装配形态**（[`Wave3Fixture`]）：真 loader（含 step 6.5 默认层）→ 真
// transport（`spawn_builtin_transport`）→ 真 handler（web / artifact / cron / lsp）→
// 真实握手。与既有 [`StartupFixture`] 的差别**只有三处**：tick 可开（本波需要「1 driver」
// 与「物理关闭后 tick 静默」两个观测面）、LSP pool 指向真 perl 假服务器（本波需要「LSP
// 工具调用成功」）、可写项目级 `.mcp.json` 与 `PERI_MCP_BUILTIN`（关闭来源分列需要）。
// 既有夹具与其全部断言逐位不变。
//
// **B. 同步维的端口替身**：链上同步的**唯一**消费面是 `AssemblyContext::lsp_pool`
// （`peri-acp-types::ports::LspPoolPort`）；本节用**计数型替身**取代宿主注入的真实 pool，
// 驱动的是**生产** `LspSyncMiddleware::after_tool` 与**真实文件读**（判据是
// `did_change` 收到的文本逐字等于磁盘内容）。槽位装/不装的判定与 `assembly.rs` 的
// `ChainSlot::Lsp` 分支同规则（`LspMiddleware` 或 `LspSyncMiddleware` 任一命中即不装）；
// 该规则本身的链级矩阵由 `assembly::tests::lsp_slot_omitted_when_instance_or_sync_closed`
// 锁定，本文件不重复断言规则，只用它装配被观测的中间件实例。
//
// **C. tick / 物理生命周期维**读既有观测面：`BuiltinInstanceSupervisor::tick_is_finished`、
// `BuiltinInstanceSupervisor::close` 的 `TickCloseOutcome` / `BuiltinServerExit`、
// pool 的 `builtin_task_count` / `builtin_tick_is_finished`（后两者是既有 `#[cfg(test)]`
// 观测面）。超时的真实执行验证由 workspace_recovery_test 承担；本组保留
// LSP 协议边界与策略关闭的独立证据。

use peri_acp_types::cron::CronTrigger;
use peri_acp_types::ports::{LspPoolPort, LspSyncError};
use peri_agent::agent::react::ToolResult;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::state::MiddlewareState;
use peri_agent::session::MessageQueue;

use crate::lsp::middleware::LspSyncMiddleware;

/// 本节全部等待的**显式上界**（真 perl 子进程 + 同进程链路，远小于各内层超时）。
const MW_BOUND: Duration = Duration::from_secs(5);
/// 静默/驱动窗口：必须 **> 2× `BUILTIN_TICK_INTERVAL`**（1s），故取 2400ms。
const MW_TICK_WINDOW: Duration = Duration::from_millis(2400);

// ─── 假 LSP 服务器（语言服务器边界的在途记账）──────────────────────────────────

/// perl 假 LSP 服务器（形态同 `lsp/tool_test.rs` 与 `mcp/builtin/lsp_test.rs` 的常量；
/// 两处都是文件内私有项，无跨 owner 复用面，故本节按同一约定抄第三份并加以记账扩展）：
///
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行 `spawned`；
/// - 对 `textDocument/documentSymbol` 请求**先**向 `$PERI_LSP_REQUESTS` 追加 `enter <id>`，
///   等 `$PERI_LSP_RELEASE` 文件出现（另有 60s 自保上限，防夹具异常时留孤儿进程）后
///   追加 `exit <id>` 再应答 —— 「在途请求数」因此是**语言服务器边界**上的可数事实，
///   不是 sleep 猜测（`enter` 行本身就是「请求已到达服务器」的 barrier）；
/// - 其它带 id 的请求（`initialize` 握手）立即回 `{"result":null}`，
///   无 id 的通知（`didOpen` / `didChange` / `didSave`）按 LSP 规范不回包。
const MW_FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    my ($id) = $b =~ /"id"\s*:\s*(\d+)/;
    next unless defined $id;
    if ($b =~ /"method"\s*:\s*"textDocument\/documentSymbol"/) {
        if (open my $f, '>>', $ENV{PERI_LSP_REQUESTS}) { print $f "enter $id\n"; close $f; }
        my $deadline = time + 60;
        while (! -e $ENV{PERI_LSP_RELEASE} && time < $deadline) { select undef, undef, undef, 0.02; }
        if (open my $f, '>>', $ENV{PERI_LSP_REQUESTS}) { print $f "exit $id\n"; close $f; }
        my $r = '{"jsonrpc":"2.0","id":' . $id . ',"result":[]}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    } else {
        my $r = '{"jsonrpc":"2.0","id":' . $id . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// `documentSymbol` 空结果的格式化文本（`lsp/formatters.rs::format_document_symbols` 的
/// 固定输出；本文件不硬编码第二份 formatter，只引用其结果）。
const MW_EMPTY_SYMBOLS_TEXT: &str = "No symbols found in document.";

/// 假 LSP 服务器的观测文件三元组（与 [`MW_FAKE_LSP_SCRIPT`] 的 env 一一对应）。
///
/// 用例 1/2（宿主装配形态的 pool）与用例 3（直建 LSP handler 链路）共用同一份配置与
/// 同一套有界等待，避免第二份夹具实现。
struct MwFakeLsp {
    /// 语言服务器边界的 `enter` / `exit` 记账文件。
    requests: PathBuf,
    /// 放行文件：存在 ⇒ 假服务器立即应答 `documentSymbol`。
    release: PathBuf,
    /// perl spawn 计数文件（0 == 从未拉起语言服务器）。
    spawn_count: PathBuf,
}

impl MwFakeLsp {
    fn new(dir: &Path) -> Self {
        Self {
            requests: dir.join("lsp_requests.txt"),
            release: dir.join("lsp_release"),
            spawn_count: dir.join("lsp_spawn_count.txt"),
        }
    }

    /// 生效配置非空的 pool（`LspServerPool::new` 惰性：此处**不**拉进程）。
    fn pool(&self, cwd: &str) -> Arc<LspServerPool> {
        let mut env = HashMap::new();
        env.insert(
            "PERI_LSP_TEST_COUNT".to_string(),
            self.spawn_count.to_string_lossy().into_owned(),
        );
        env.insert(
            "PERI_LSP_REQUESTS".to_string(),
            self.requests.to_string_lossy().into_owned(),
        );
        env.insert(
            "PERI_LSP_RELEASE".to_string(),
            self.release.to_string_lossy().into_owned(),
        );
        Arc::new(LspServerPool::new(
            cwd,
            LspConfigFile {
                lsp_servers: HashMap::from([(
                    "mw-fake-lsp".to_string(),
                    LspServerConfig {
                        name: "mw-fake-lsp".to_string(),
                        command: "perl".to_string(),
                        args: vec!["-e".to_string(), MW_FAKE_LSP_SCRIPT.to_string()],
                        env: Some(env),
                        extension_to_language: HashMap::from([(
                            ".rs".to_string(),
                            "rust".to_string(),
                        )]),
                        initialization_options: None,
                        disabled: None,
                        max_restarts: Some(3),
                        startup_timeout: None,
                        source: None,
                    },
                )]),
            },
        ))
    }

    /// 放行假服务器（存在该文件 ⇒ `documentSymbol` 立即应答）。
    fn open_release(&self) {
        std::fs::write(&self.release, b"go").expect("放行文件可写");
    }

    /// 关闭放行（删除文件 ⇒ `documentSymbol` 请求停在服务器侧 = 在途）。
    fn close_release(&self) {
        let _ = std::fs::remove_file(&self.release);
    }

    /// 已 spawn 的语言服务器进程数（0 == 从未拉起）。
    fn spawns(&self) -> usize {
        std::fs::read_to_string(&self.spawn_count)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// 语言服务器边界记账：`(enter 行数, exit 行数)`。差值 = 在途请求数。
    fn accounting(&self) -> (usize, usize) {
        mw_lsp_accounting(&self.requests)
    }

    /// 有界等待记账到达期望值。
    ///
    /// 判据是**语言服务器自己写下的 barrier 行**，不是 sleep；`MW_BOUND` 到期即失败并打印
    /// 期望/实际（phase 标注由 `label` 给出）。
    async fn await_accounting(&self, label: &str, expected: (usize, usize)) {
        mw_await_lsp_accounting(&self.requests, label, expected).await;
    }

    /// 记账文件路径（需要在并行 future 里等待 barrier 的调用方使用）。
    fn requests_path(&self) -> PathBuf {
        self.requests.clone()
    }
}

/// [`MwFakeLsp`] 记账的有界等待（同函数体的自由函数形态：并行 future 里只需路径）。
async fn mw_await_lsp_accounting(requests: &Path, label: &str, expected: (usize, usize)) {
    let started = std::time::Instant::now();
    loop {
        let actual = mw_lsp_accounting(requests);
        if actual == expected {
            return;
        }
        assert!(
            started.elapsed() < MW_BOUND,
            "[{label}] 语言服务器记账未在 {MW_BOUND:?} 内到达期望 {expected:?}，实际 {actual:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 语言服务器边界记账：`(enter 行数, exit 行数)`。差值 = 在途请求数。
fn mw_lsp_accounting(requests: &Path) -> (usize, usize) {
    let text = std::fs::read_to_string(requests).unwrap_or_default();
    (
        text.lines()
            .filter(|line| line.starts_with("enter "))
            .count(),
        text.lines()
            .filter(|line| line.starts_with("exit "))
            .count(),
    )
}

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

/// 真宿主装配形态的 pool + cron 驱动观测面 + 语言服务器在途记账。
struct Wave3Fixture {
    _fixture: tempfile::TempDir,
    _env: LoaderEnvGuard,
    pool: Arc<McpClientPool>,
    project: PathBuf,
    scheduler: Arc<parking_lot::Mutex<CronScheduler>>,
    triggers: tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
    /// 假语言服务器的观测面（记账 / 放行 / spawn 计数）。
    lsp: MwFakeLsp,
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

        let lsp = MwFakeLsp::new(fixture.path());
        let lsp_pool = lsp.pool(&cwd);

        let pool = Arc::new(McpClientPool::new_pending());
        let (cron_trigger_tx, triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(cron_trigger_tx)));
        // A33：实例上下文由宿主装配在 `run_initialize` **之前**注入（本夹具扮演宿主）。
        pool.set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new(cwd)
                .with_cron(CronInstanceInput {
                    scheduler: Arc::clone(&scheduler),
                    tick_enabled: spec.tick_enabled,
                })
                .with_lsp(LspInstanceInput { pool: lsp_pool }),
        ))
        .expect("夹具首次注入上下文必须成功");
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::run_initialize(pool.clone(), &project, &home, status_tx, None, None).await;

        Self {
            _fixture: fixture,
            _env: env,
            pool,
            project,
            scheduler,
            triggers,
            lsp,
        }
    }

    /// 放行假服务器（存在该文件 ⇒ `documentSymbol` 立即应答）。
    fn open_lsp_release(&self) {
        self.lsp.open_release();
    }

    /// 已 spawn 的语言服务器进程数（0 == 从未拉起）。
    fn lsp_spawns(&self) -> usize {
        self.lsp.spawns()
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

// ─── 同步维：计数型端口替身 + 最小 hook 态 ───────────────────────────────────────

/// 端口调用轨迹（顺序敏感）：ready 探测 / 内容变更 / 保存。
#[derive(Debug, Clone, PartialEq, Eq)]
enum MwSyncStep {
    Ready(PathBuf),
    Change { path: PathBuf, text: String },
    Save(PathBuf),
}

/// 计数型 `LspPoolPort` 替身（**唯一**消费面：`AssemblyContext::lsp_pool` 端口位）。
///
/// `ready` 可配置：false ⇒ 读文件前置门关闭，`did_change` / `did_save` 都不得被调用
/// （这正是「`ready_for` 先于读文件」的可证伪形态）。三个方法各自计数并记录轨迹。
struct MwSyncPort {
    ready: bool,
    steps: Mutex<Vec<MwSyncStep>>,
    ready_calls: AtomicUsize,
    change_calls: AtomicUsize,
    save_calls: AtomicUsize,
}

impl MwSyncPort {
    fn new(ready: bool) -> Self {
        Self {
            ready,
            steps: Mutex::new(Vec::new()),
            ready_calls: AtomicUsize::new(0),
            change_calls: AtomicUsize::new(0),
            save_calls: AtomicUsize::new(0),
        }
    }

    fn steps(&self) -> Vec<MwSyncStep> {
        self.steps.lock().unwrap().clone()
    }

    /// `(ready_for, did_change, did_save)` 调用计数。
    fn counts(&self) -> (usize, usize, usize) {
        (
            self.ready_calls.load(Ordering::SeqCst),
            self.change_calls.load(Ordering::SeqCst),
            self.save_calls.load(Ordering::SeqCst),
        )
    }

    /// 「读文件发生过」的判据：`did_change` 收到的文本（端口**不读文件**，内容只可能来自
    /// 中间件读盘后传入）。
    fn synced_text(&self) -> Option<String> {
        self.steps().into_iter().find_map(|step| match step {
            MwSyncStep::Change { text, .. } => Some(text),
            _ => None,
        })
    }
}

#[async_trait]
impl LspPoolPort for MwSyncPort {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {}

    fn ready_for(&self, path: &Path) -> bool {
        self.ready_calls.fetch_add(1, Ordering::SeqCst);
        self.steps
            .lock()
            .unwrap()
            .push(MwSyncStep::Ready(path.to_path_buf()));
        self.ready
    }

    async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspSyncError> {
        self.change_calls.fetch_add(1, Ordering::SeqCst);
        self.steps.lock().unwrap().push(MwSyncStep::Change {
            path: path.to_path_buf(),
            text: text.to_string(),
        });
        Ok(())
    }

    async fn did_save(&self, path: &Path) -> Result<(), LspSyncError> {
        self.save_calls.fetch_add(1, Ordering::SeqCst);
        self.steps
            .lock()
            .unwrap()
            .push(MwSyncStep::Save(path.to_path_buf()));
        Ok(())
    }
}

/// 最小 hook 态：`after_tool` 只用 `StateView::cwd()`（形态同 `lsp::middleware::tests`
/// 的 `TestState`；本文件只保留被观测路径需要的最小面）。
struct MwHookState {
    cwd: String,
    messages: Vec<BaseMessage>,
    queue: MessageQueue,
    recall: Vec<String>,
}

impl MwHookState {
    fn new(cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
            messages: Vec::new(),
            queue: MessageQueue::new(),
            recall: Vec::new(),
        }
    }
}

impl MiddlewareState for MwHookState {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    fn add_message(&mut self, message: BaseMessage) {
        self.messages.push(message);
    }

    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        true
    }

    fn current_step(&self) -> usize {
        0
    }

    fn push_recall(&mut self, item: String) {
        self.recall.push(item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall)
    }

    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

/// 同步槽位装/不装的判定：**事实源**是 `peri-middlewares/src/assembly.rs` 的
/// `ChainSlot::Lsp` 分支——`LspMiddleware`（builtin 实例 policy key）或
/// `LspSyncMiddleware`（链槽位名）任一命中即不装同步中间件。
///
/// 本函数只用于**装配被观测的中间件实例**；该规则本身的四条链级组合由
/// `assembly::tests::lsp_slot_omitted_when_instance_or_sync_closed` 断言，本文件不重复。
fn mw_sync_slot_mounted(disabled: &HashSet<String>) -> bool {
    !disabled.contains("LspMiddleware") && !disabled.contains("LspSyncMiddleware")
}

/// 跑一次 `after_tool`（`Write` 工具、工具结果固定为成功——同步恒不得改写它）。
async fn mw_run_write_sync(
    port: &Arc<MwSyncPort>,
    state: &mut MwHookState,
    file_path: &Path,
) -> peri_agent::error::AgentResult<()> {
    let middleware = LspSyncMiddleware::new(Arc::clone(port) as Arc<dyn LspPoolPort>);
    let tool_call = ToolCall::new(
        "call-mw-sync",
        crate::tool_search::core_tools::TOOL_WRITE,
        json!({ "file_path": file_path.to_string_lossy() }),
    );
    let result = ToolResult::success(
        "call-mw-sync",
        crate::tool_search::core_tools::TOOL_WRITE,
        "工具原始结果",
    );
    middleware.after_tool(state, &tool_call, &result).await
}

/// 链工具集合（可观察能力面，**带 direct 标志**）：`McpMiddleware::collect_tools` 的产出。
///
/// 与既有 [`collected_tool_names`] 同一装配方式（`with_builtin_closures` 注入关闭集）；
/// 多返回 `is_direct()`，使「cron / lsp 一律 deferred」与「出现/消失」在同一次收集中可断。
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

#[path = "builtin_runtime_lsp_test.rs"]
mod lsp;
