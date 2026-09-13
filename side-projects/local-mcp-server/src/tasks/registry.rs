//! 唯一任务注册表：owner 绑定、容量、TTL、停止与关闭清理。
//!
//! 单进程形态（D-003）下不存在"broker 表 + worker 表"的双份状态：本模块是
//! **唯一**的任务表，直接持有 [`BashTask`] 句柄（进程、进程组、日志句柄与实时状态），
//! 进程的真身在 [`crate::tasks::bash::BashTasks`]。二者之间是进程内普通调用：
//! 没有信封、没有帧、没有请求配对、没有轮询式 RPC 刷新。
//!
//! - 铸造 `task_id`（`shell-<UUIDv7 完整>`，禁止截断）与高熵不透明日志句柄；
//! - 以 `(principal, client_instance)` 绑定每个任务，跨主体/跨连接实例一律拒绝；
//! - 维护容量（并发上限）、终态保留（默认 TTL 1h 或最近 100 条），到期标记 `Gone`
//!   并回收日志；
//! - **MCP 可访问**的只有查询面：`sandbox://tasks`、`sandbox://tasks/{task_id}`
//!   资源（经 [`crate::mcp::resources::TaskStatusSource`] 实现转发到
//!   [`TaskRegistry::snapshots_for`]/[`TaskRegistry::snapshot_for`]）。停止与读日志
//!   的 MCP 可访问路径是**普通 Bash 命令**：`kill`/`kill -- -<pgid>` 停进程组，
//!   `Read` 工具或 Bash 读快照里的日志路径（returned 文本同带 `kill` 指引）。
//!
//! 其余方法（[`TaskRegistry::stop`]、[`TaskRegistry::read_log`]、
//! [`TaskRegistry::read_output`]）是**同进程内嵌 API**：由集成测试与嵌入方使用，
//! 不在 MCP wire 上暴露（七工具 schema 冻结，资源只读；新增工具或字段会破坏
//! R-002 的公开契约），因此不要把它们当作调用方可依赖的控制面。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::error::WorkerError;
use crate::tasks::bash::{BashTask, BashTaskError, BashTasks};
use crate::tasks::log::{mint_log_handle, LogStream, OutputPersist};
use crate::tasks::params::{
    BashMode, BashStartPayload, BashTaskState, TaskLogPayload, MAX_LOG_READ_BYTES,
};
use crate::tools::bash::limits::{merge_output, truncate_output};
use crate::wire::{
    ClientInstanceId, PrincipalId, RequestContext, TaskId, TaskSnapshot, TaskStatus,
};

/// 任务列表资源 URI。
pub const TASKS_RESOURCE_URI: &str = "sandbox://tasks";

/// 单任务资源 URI（`sandbox://tasks/{task_id}`）。
pub fn task_resource_uri(task_id: &str) -> String {
    format!("{TASKS_RESOURCE_URI}/{task_id}")
}

/// 可注入时钟（TTL 与时间戳都经此取得，便于测试确定性）。
pub trait Clock: Send + Sync {
    /// 墙上时间（用于 `started_at`/`ended_at` 的 RFC 3339）。
    fn wall(&self) -> DateTime<Utc>;
    /// 单调时间（用于 TTL 计算）。
    fn monotonic(&self) -> Instant;
}

/// 生产时钟。
pub struct SystemClock;

impl Clock for SystemClock {
    fn wall(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}

/// 注册表配置。
#[derive(Clone)]
pub struct TaskRegistryConfig {
    /// 并发运行任务上限（源：`BackgroundTaskRegistry::SHELL_LIMIT = 5`）。
    pub shell_limit: usize,
    /// 终态保留时长（默认 1h）。
    pub terminal_ttl: std::time::Duration,
    /// 终态最多保留条数（默认 100）。
    pub max_terminal_entries: usize,
    /// 后台任务状态轮询间隔；`None` = 不自动轮询（测试或由协议层显式驱动）。
    ///
    /// 轮询只读进程内状态（无 RPC），因此它与"状态面变化的最大延迟"同义。
    pub poll_interval: Option<std::time::Duration>,
    /// 单次日志读取字节上限。
    pub log_read_bytes: usize,
    /// 截断输出落盘 sink（与 Bash 日志同一私有目录约定）。
    pub persist: Arc<dyn OutputPersist>,
}

impl TaskRegistryConfig {
    /// 默认配置：并发 5、终态 1h/100 条、轮询 250ms、日志读取 64 KiB。
    ///
    /// 任务的工作目录不在这里：cwd 的**唯一来源**是 [`BashTasks::workspace`]
    /// （即启动参数指定的宿主工作区根），注册表不维护第二份路径事实。
    pub fn new(persist: Arc<dyn OutputPersist>) -> Self {
        Self {
            shell_limit: 5,
            terminal_ttl: std::time::Duration::from_secs(3600),
            max_terminal_entries: 100,
            // 与协议层资源订阅的观察间隔（250ms）对齐，状态面变化的最大延迟即此值。
            poll_interval: Some(std::time::Duration::from_millis(250)),
            log_read_bytes: MAX_LOG_READ_BYTES,
            persist,
        }
    }
}

/// `Bash` 工具传入的运行参数（已按源语义解析 timeout）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRunArgs {
    /// 原始命令。
    pub command: String,
    /// 解析后的 timeout（毫秒）：`None` = 不超时。
    pub timeout_ms: Option<u64>,
    /// 是否显式后台。
    pub background: bool,
}

/// 前台调用的结局。
#[derive(Debug, Clone, PartialEq)]
pub enum BashRun {
    /// 前台在期限内结束（任务不进注册表）。
    Finished {
        /// 任务终态（装箱以平衡枚举变体大小）。
        state: Box<BashTaskState>,
    },
    /// 显式后台启动，或前台超时提升为后台任务（已登记，owner 可见）。
    Running {
        /// 已登记的 owner 绑定快照（装箱以平衡枚举变体大小）。
        snapshot: Box<TaskSnapshot>,
        /// 进程的运行中状态（含已捕获的部分输出与进程状态快照）。
        state: Box<BashTaskState>,
        /// 是否由前台超时提升而来。
        promoted: bool,
    },
}

/// 启动失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BashRunError {
    /// 并发上限已满（显式后台启动前检查）。
    #[error("Maximum {limit} concurrent background tasks reached")]
    ConcurrentLimit {
        /// 上限值。
        limit: usize,
    },
    /// 前台超时但无法提升为后台任务（容量已满）；进程组已终止。
    #[error("{reason}")]
    PromotionUnavailable {
        /// 源文案 `Maximum {limit} concurrent background tasks reached`。
        reason: String,
        /// 提升失败前的运行中状态（用于源格式超时文案；装箱以平衡枚举大小）。
        state: Box<BashTaskState>,
    },
    /// 任务层报告的业务失败（例如 spawn 失败），文本保留给工具层。
    #[error("{message}")]
    WorkerFailure {
        /// 稳定错误类。
        kind: String,
        /// 已脱敏文本。
        message: String,
    },
    /// 内部状态不一致（例如重复任务 id）。
    #[error("task payload error: {message}")]
    Payload {
        /// 说明。
        message: String,
    },
    /// 执行面失败（fail closed，无宿主回退）。
    #[error(transparent)]
    Worker(#[from] WorkerError),
}

/// 任务访问失败。对外文本不区分"不存在"与"不属于你"。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskAccessError {
    /// 从未登记过该 id。
    #[error("unknown task: {task_id}")]
    Unknown {
        /// 任务 id。
        task_id: String,
    },
    /// 属于其他主体。
    #[error("unknown task: {task_id}")]
    ForeignOwner {
        /// 任务 id。
        task_id: String,
    },
    /// 同一主体但不同连接实例。
    #[error("unknown task: {task_id}")]
    ForeignInstance {
        /// 任务 id。
        task_id: String,
    },
    /// 已过 TTL / 超出保留条数被回收。
    #[error("task expired: {task_id}")]
    Gone {
        /// 任务 id。
        task_id: String,
    },
    /// 任务层报告的失败（句柄不匹配、日志不可读等）。
    #[error("task unavailable: {message}")]
    WorkerFailure {
        /// 稳定错误类。
        kind: String,
        /// 已脱敏文本。
        message: String,
    },
    /// 执行面失败。
    #[error(transparent)]
    Worker(#[from] WorkerError),
}

impl From<BashTaskError> for TaskAccessError {
    fn from(error: BashTaskError) -> Self {
        Self::WorkerFailure {
            kind: "protocol".to_string(),
            message: error.to_string(),
        }
    }
}

impl From<BashTaskError> for BashRunError {
    fn from(error: BashTaskError) -> Self {
        Self::WorkerFailure {
            kind: "protocol".to_string(),
            message: error.to_string(),
        }
    }
}

impl TaskAccessError {
    /// 可外发文本：不泄露其他主体的任务是否存在。
    pub fn public_message(&self) -> String {
        self.to_string()
    }
}

/// 任务输出（合并 + 截断后的文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutput {
    /// 合并后的输出（stdout + `[stderr]` 段 + 退出码行）。
    pub text: String,
    /// 是否被截断。
    pub truncated: bool,
    /// 退出码。
    pub exit_code: Option<i32>,
    /// 当前状态。
    pub status: TaskStatus,
}

/// 任务事件（协议层据此发布 `notifications/resources/updated` 等）。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskEvent {
    /// 事件类型。
    pub kind: TaskEventKind,
    /// 事件发生时的快照。
    pub snapshot: TaskSnapshot,
}

/// 任务事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEventKind {
    /// 任务已登记（后台启动或前台提升）。
    Started,
    /// 状态更新（仍在运行或状态字段变化）。
    Updated,
    /// 进入终态。
    Terminal,
    /// 记录被回收（`Gone`）。
    Retired,
}

/// 注册表条目：归属元数据 + **唯一**的进程句柄。
struct TaskEntry {
    task_id: TaskId,
    log_handle: String,
    owner: PrincipalId,
    client_instance: ClientInstanceId,
    promoted: bool,
    /// 进程生命周期的唯一句柄（状态从它读取，不复制）。
    task: Arc<BashTask>,
    terminal_at: Option<Instant>,
    terminal_wall: Option<DateTime<Utc>>,
    retired: bool,
}

/// 通过授权检查的任务句柄。
struct AuthorizedTask {
    task: Arc<BashTask>,
    log_handle: String,
    owner: PrincipalId,
    client_instance: ClientInstanceId,
}

const EVENT_CHANNEL_CAPACITY: usize = 64;
const RETIRED_MEMORY: usize = 256;
const COMMAND_SUMMARY_CHARS: usize = 80;

/// 唯一任务注册表。
pub struct TaskRegistry {
    /// 进程执行器（spawn 与日志都经它；任务表在本结构内）。
    bash: Arc<BashTasks>,
    clock: Arc<dyn Clock>,
    config: TaskRegistryConfig,
    tasks: Mutex<BTreeMap<TaskId, TaskEntry>>,
    retired: Mutex<VecDeque<TaskId>>,
    events: broadcast::Sender<TaskEvent>,
}

impl TaskRegistry {
    /// 新建注册表；配置 `poll_interval` 时启动后台状态轮询（只读进程内状态）。
    pub fn new(
        bash: Arc<BashTasks>,
        clock: Arc<dyn Clock>,
        config: TaskRegistryConfig,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let registry = Arc::new(Self {
            bash,
            clock,
            config,
            tasks: Mutex::new(BTreeMap::new()),
            retired: Mutex::new(VecDeque::new()),
            events,
        });
        if let Some(interval) = registry.config.poll_interval {
            let weak = Arc::downgrade(&registry);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    match weak.upgrade() {
                        Some(registry) => {
                            registry.refresh_all().await;
                            registry.reap_expired();
                        }
                        None => break,
                    }
                }
            });
        }
        registry
    }

    /// 订阅任务事件（协议层通知出口）。
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    /// 配置引用。
    pub fn config(&self) -> &TaskRegistryConfig {
        &self.config
    }

    /// 进程执行器句柄（诊断与测试使用）。
    pub fn bash(&self) -> &Arc<BashTasks> {
        &self.bash
    }

    fn running_count(&self) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|entry| !entry.task.is_terminal())
            .count()
    }

    /// 启动一次 `Bash`（前台或后台）。
    ///
    /// - 显式后台：先占容量（满则 [`BashRunError::ConcurrentLimit`]），再 spawn。
    /// - 前台：spawn 后等待到终态或 timeout；到期后进程继续存活，此时尝试占容量，
    ///   成功则登记为后台任务，失败则终止进程组并返回
    ///   [`BashRunError::PromotionUnavailable`]（源同文案语义）。
    pub async fn start_bash(
        &self,
        ctx: &RequestContext,
        args: BashRunArgs,
    ) -> Result<BashRun, BashRunError> {
        self.reap_expired();
        if args.background && self.running_count() >= self.config.shell_limit {
            return Err(BashRunError::ConcurrentLimit {
                limit: self.config.shell_limit,
            });
        }

        let task_id = mint_task_id();
        let log_handle = mint_log_handle();
        let mode = if args.background {
            BashMode::Background
        } else {
            BashMode::Foreground
        };
        let payload = BashStartPayload {
            task_id: task_id.clone(),
            log_handle: log_handle.clone(),
            command: args.command.clone(),
            cwd: self.bash.workspace().to_string_lossy().to_string(),
            mode,
            timeout_ms: args.timeout_ms,
            await_ms: if args.background {
                None
            } else {
                args.timeout_ms
            },
        };

        // 前台等待期间的请求取消会终止进程组（与源实现的取消语义一致）。
        let cancel = if args.background {
            None
        } else {
            Some(ctx.cancellation.clone())
        };
        let task = self.bash.start(payload, cancel).await?;
        let state = task.snapshot();

        if state.status.is_terminal() {
            return Ok(BashRun::Finished {
                state: Box::new(state),
            });
        }

        // 运行中：显式后台直接登记；前台需先取得容量（提升）。
        let promoted = !args.background;
        if promoted && self.running_count() >= self.config.shell_limit {
            let reason = format!(
                "Maximum {} concurrent background tasks reached",
                self.config.shell_limit
            );
            // 提升失败：终止进程组，避免留下无人管理的进程。
            task.terminate_group(false).await;
            return Err(BashRunError::PromotionUnavailable {
                reason,
                state: Box::new(state),
            });
        }

        let entry_state = state;
        let snapshot = {
            let mut tasks = self.tasks.lock();
            let entry = TaskEntry {
                task_id: task_id.clone(),
                log_handle,
                owner: ctx.principal.clone(),
                client_instance: ctx.client_instance.clone(),
                promoted,
                task: Arc::clone(&task),
                terminal_at: None,
                terminal_wall: None,
                retired: false,
            };
            let snapshot = snapshot_from(&entry, &entry_state);
            if tasks.insert(task_id.clone(), entry).is_some() {
                return Err(BashRunError::Payload {
                    message: "duplicate task id".to_string(),
                });
            }
            snapshot
        };
        self.emit(TaskEventKind::Started, snapshot.clone());
        Ok(BashRun::Running {
            snapshot: Box::new(snapshot),
            state: Box::new(entry_state),
            promoted,
        })
    }

    /// 列出当前主体与连接实例可见的任务快照（`sandbox://tasks`）。
    pub fn list(&self, ctx: &RequestContext) -> Vec<TaskSnapshot> {
        self.tasks
            .lock()
            .values()
            .filter(|entry| {
                entry.owner == ctx.principal && entry.client_instance == ctx.client_instance
            })
            .map(|entry| self.snapshot_of(entry))
            .collect()
    }

    /// 只读快照查询（协议层资源面使用）：按主体 + 连接实例过滤，跨身份一律返回空。
    ///
    /// 与 [`Self::list`] 的差别是"无错误通道"：调用方（`resources/read`）拿到的
    /// 结果与"任务不存在"不可区分，因此不会泄露他人任务是否存在。
    pub fn snapshots_for(&self, principal: &str, client_instance: &str) -> Vec<TaskSnapshot> {
        self.tasks
            .lock()
            .values()
            .filter(|entry| entry.owner == principal && entry.client_instance == client_instance)
            .map(|entry| self.snapshot_of(entry))
            .collect()
    }

    /// 只读单任务快照（协议层资源面使用）：不存在或不属于该身份时返回 `None`。
    pub fn snapshot_for(
        &self,
        principal: &str,
        client_instance: &str,
        task_id: &str,
    ) -> Option<TaskSnapshot> {
        self.tasks
            .lock()
            .values()
            .find(|entry| {
                entry.task_id == task_id
                    && entry.owner == principal
                    && entry.client_instance == client_instance
            })
            .map(|entry| self.snapshot_of(entry))
    }

    /// 单任务快照（`sandbox://tasks/{task_id}`）；跨身份读取拒绝。
    pub fn snapshot(
        &self,
        ctx: &RequestContext,
        task_id: &str,
    ) -> Result<TaskSnapshot, TaskAccessError> {
        let authorized = self.authorize(ctx, task_id)?;
        Ok(snapshot_of(&authorized, task_id))
    }

    /// 拉取一次最新状态（进程内直读；终态转移会更新 TTL 基准并发出事件）。
    pub async fn refresh(
        &self,
        ctx: &RequestContext,
        task_id: &str,
    ) -> Result<TaskSnapshot, TaskAccessError> {
        self.authorize(ctx, task_id)?;
        self.apply_task_state(task_id)
    }

    /// 刷新全部**尚未完成终态记账**的任务（协议层或轮询器调用）。
    ///
    /// 判据是"条目还没有 `terminal_at`"，而**不是**"任务看起来还在运行"：
    /// 任务自身的状态由它在进程退出时立刻翻成终态，可能发生在两次轮询之间，
    /// 因此按 `!is_terminal()` 过滤会漏掉这些完成事件——`terminal_at` 永远为空，
    /// TTL 与保留条数（[`TaskRegistry::reap_expired`] 的前提）就永远不生效，
    /// `Terminal` 事件也不会发出。按"未记账"过滤保证每个任务恰好被观察一次终态转移。
    pub async fn refresh_all(&self) {
        let pending: Vec<TaskId> = self
            .tasks
            .lock()
            .values()
            .filter(|entry| entry.terminal_at.is_none())
            .map(|entry| entry.task_id.clone())
            .collect();
        for task_id in pending {
            if let Err(error) = self.apply_task_state(&task_id) {
                tracing::warn!(task_id = %task_id, error = %error, "任务状态刷新失败");
            }
        }
    }

    /// 显式停止（TERM → 2s → KILL）；重复停止幂等返回终态。
    ///
    /// **内嵌 API**：调用方来自同进程（集成测试、嵌入方、[`TaskRegistry::close`] 的
    /// 同类路径），MCP wire 上不可达——调用方在 MCP 上停止任务用普通 Bash
    /// `kill -- -<pgid>`（作用于同一进程组；显式停止与外部 `kill` 同走进程组信号，
    /// 进程以非零/信号退出收敛为 `failed`，只有 KILL 升级后仍未观察到回收时才如实
    /// 标 `killed`——F-P3-01 按 `tests/tasks_registry.rs` 的实测行为订正，两者都不隐藏退出码）。
    pub async fn stop(
        &self,
        ctx: &RequestContext,
        task_id: &str,
    ) -> Result<TaskSnapshot, TaskAccessError> {
        let authorized = self.authorize(ctx, task_id)?;
        if authorized.task.is_terminal() {
            return self.snapshot(ctx, task_id);
        }
        authorized.task.terminate_group(false).await;
        self.apply_task_state(task_id)
    }

    /// 读取任务日志尾部（跨身份拒绝；句柄由注册表持有，调用方无需提供）。
    ///
    /// **内嵌 API**（同 [`TaskRegistry::stop`] 的边界）：MCP 上的日志路径是快照里的
    /// `stdout_log`/`stderr_log`，用 `Read` 工具或 Bash 直接读文件。
    pub async fn read_log(
        &self,
        ctx: &RequestContext,
        task_id: &str,
        stream: LogStream,
        max_bytes: usize,
    ) -> Result<TaskLogPayload, TaskAccessError> {
        let authorized = self.authorize(ctx, task_id)?;
        self.bash
            .read_log(&authorized.task, &authorized.log_handle, stream, max_bytes)
            .map_err(TaskAccessError::from)
    }

    /// 读取合并后的任务输出（stdout + `[stderr]` 段 + 退出码行），供资源/通知使用。
    ///
    /// **内嵌 API**（同 [`TaskRegistry::stop`] 的边界）：当前无 MCP 调用方，保留给
    /// 同进程消费者与集成测试，避免把不可达面写成调用方可依赖的控制面。
    pub async fn read_output(
        &self,
        ctx: &RequestContext,
        task_id: &str,
    ) -> Result<TaskOutput, TaskAccessError> {
        let authorized = self.authorize(ctx, task_id)?;
        let state = authorized.task.snapshot();
        let max_bytes = self.config.log_read_bytes;
        let stdout = self.bash.read_log(
            &authorized.task,
            &authorized.log_handle,
            LogStream::Stdout,
            max_bytes,
        )?;
        let stderr = self.bash.read_log(
            &authorized.task,
            &authorized.log_handle,
            LogStream::Stderr,
            max_bytes,
        )?;
        let merged = merge_output(&stdout.content, &stderr.content, state.exit_code);
        let read_truncated = stdout.truncated || stderr.truncated;
        let shaped = truncate_output(&merged, self.config.persist.as_ref());
        Ok(TaskOutput {
            text: shaped.text,
            truncated: shaped.truncated || read_truncated,
            exit_code: state.exit_code,
            status: state.status,
        })
    }

    /// 回收超期终态记录（TTL 或保留条数），返回被标记 `Gone` 的任务 id。
    ///
    /// TTL 只使用单调时钟（不随墙上时间跳变）；保留条数按结束时间从旧到新回收。
    /// 记录被回收时**同时回收日志文件**（`Gone` 的对外语义就是"日志已被回收"）。
    pub fn reap_expired(&self) -> Vec<TaskId> {
        let now = self.clock.monotonic();
        let ttl = self.config.terminal_ttl;

        let mut retired_now: Vec<TaskId> = Vec::new();
        let mut forgotten: Vec<Arc<BashTask>> = Vec::new();
        {
            let mut tasks = self.tasks.lock();
            // 1) TTL 到期。
            for entry in tasks.values() {
                if entry.retired {
                    continue;
                }
                if let Some(terminal_at) = entry.terminal_at {
                    if now.saturating_duration_since(terminal_at) >= ttl {
                        retired_now.push(entry.task_id.clone());
                    }
                }
            }
            // 2) 保留条数上限：终态条目按结束时间从旧到新回收。
            let mut terminal_entries: Vec<(TaskId, DateTime<Utc>)> = tasks
                .values()
                .filter(|entry| !entry.retired && entry.terminal_at.is_some())
                .map(|entry| {
                    (
                        entry.task_id.clone(),
                        entry.terminal_wall.unwrap_or_else(|| self.clock.wall()),
                    )
                })
                .collect();
            if terminal_entries.len() > self.config.max_terminal_entries {
                terminal_entries.sort_by_key(|(_, ended_at)| *ended_at);
                let overflow = terminal_entries.len() - self.config.max_terminal_entries;
                for (task_id, _) in terminal_entries.into_iter().take(overflow) {
                    if !retired_now.contains(&task_id) {
                        retired_now.push(task_id);
                    }
                }
            }
            // 3) 标记并移除条目；`Gone` 由 retired 记忆集合回答。
            for task_id in &retired_now {
                if let Some(entry) = tasks.get_mut(task_id) {
                    entry.retired = true;
                }
            }
            for task_id in &retired_now {
                if let Some(entry) = tasks.remove(task_id) {
                    let mut snapshot = self.snapshot_of(&entry);
                    snapshot.status = TaskStatus::Gone;
                    forgotten.push(Arc::clone(&entry.task));
                    self.retired.lock().push_back(task_id.clone());
                    self.emit(TaskEventKind::Retired, snapshot);
                }
            }
            while self.retired.lock().len() > RETIRED_MEMORY {
                self.retired.lock().pop_front();
            }
        }
        // 日志回收在表锁之外做（文件 IO 不阻塞查询面）。
        for task in forgotten {
            self.bash.forget(&task);
        }
        retired_now
    }

    /// 关闭：停止所有运行中任务、回收全部记录，并终止其进程组。
    pub async fn close(&self) -> Result<(), WorkerError> {
        let running: Vec<Arc<BashTask>> = self
            .tasks
            .lock()
            .values()
            .filter(|entry| !entry.task.is_terminal())
            .map(|entry| Arc::clone(&entry.task))
            .collect();
        self.bash.shutdown(&running).await;
        Ok(())
    }

    /// 授权检查：主体 + 连接实例必须同时匹配。
    fn authorize(
        &self,
        ctx: &RequestContext,
        task_id: &str,
    ) -> Result<AuthorizedTask, TaskAccessError> {
        let tasks = self.tasks.lock();
        let entry = match tasks.get(task_id) {
            Some(entry) => entry,
            None => {
                return if self.retired.lock().iter().any(|id| id == task_id) {
                    Err(TaskAccessError::Gone {
                        task_id: task_id.to_string(),
                    })
                } else {
                    Err(TaskAccessError::Unknown {
                        task_id: task_id.to_string(),
                    })
                };
            }
        };
        if entry.owner != ctx.principal {
            return Err(TaskAccessError::ForeignOwner {
                task_id: task_id.to_string(),
            });
        }
        if entry.client_instance != ctx.client_instance {
            return Err(TaskAccessError::ForeignInstance {
                task_id: task_id.to_string(),
            });
        }
        Ok(AuthorizedTask {
            task: Arc::clone(&entry.task),
            log_handle: entry.log_handle.clone(),
            owner: entry.owner.clone(),
            client_instance: entry.client_instance.clone(),
        })
    }

    fn snapshot_of(&self, entry: &TaskEntry) -> TaskSnapshot {
        snapshot_from(entry, &entry.task.snapshot())
    }

    /// 读取任务当前状态并更新条目（终态转移记录 TTL 基准并发出事件）。
    fn apply_task_state(&self, task_id: &str) -> Result<TaskSnapshot, TaskAccessError> {
        let state = {
            let tasks = self.tasks.lock();
            let entry = tasks.get(task_id).ok_or_else(|| {
                if self.retired.lock().iter().any(|id| id == task_id) {
                    TaskAccessError::Gone {
                        task_id: task_id.to_string(),
                    }
                } else {
                    TaskAccessError::Unknown {
                        task_id: task_id.to_string(),
                    }
                }
            })?;
            entry.task.snapshot()
        };
        let (snapshot, kind) = {
            let mut tasks = self.tasks.lock();
            let entry = tasks
                .get_mut(task_id)
                .ok_or_else(|| TaskAccessError::Unknown {
                    task_id: task_id.to_string(),
                })?;
            entry.promoted = state.promoted;
            let snapshot = snapshot_from(entry, &state);
            let just_terminal = state.status.is_terminal() && entry.terminal_at.is_none();
            if just_terminal {
                entry.terminal_at = Some(self.clock.monotonic());
                entry.terminal_wall = Some(self.clock.wall());
            }
            let kind = if state.status.is_terminal() {
                if just_terminal {
                    TaskEventKind::Terminal
                } else {
                    TaskEventKind::Updated
                }
            } else {
                TaskEventKind::Updated
            };
            (snapshot, kind)
        };
        self.emit(kind, snapshot.clone());
        Ok(snapshot)
    }

    fn emit(&self, kind: TaskEventKind, snapshot: TaskSnapshot) {
        let _ = self.events.send(TaskEvent { kind, snapshot });
    }
}

/// 构造对外快照：归属来自条目，进程事实来自任务自身（单一事实源）。
fn snapshot_from(entry: &TaskEntry, state: &BashTaskState) -> TaskSnapshot {
    snapshot_with(
        entry.task_id.clone(),
        &entry.owner,
        &entry.client_instance,
        state,
    )
}

/// 构造对外快照（授权路径下条目信息由 `AuthorizedTask` 提供）。
fn snapshot_with(
    task_id: TaskId,
    owner: &PrincipalId,
    client_instance: &ClientInstanceId,
    state: &BashTaskState,
) -> TaskSnapshot {
    TaskSnapshot {
        task_id,
        owner: owner.clone(),
        client_instance: client_instance.clone(),
        status: state.status,
        pid: state.pid,
        pgid: state.pgid,
        stdout_log: state.stdout_log.clone(),
        stderr_log: state.stderr_log.clone(),
        exit_code: state.exit_code,
        started_at: state.started_at.clone(),
        ended_at: state.ended_at.clone(),
    }
}

/// 按授权结果构造快照（任务事实直接来自唯一进程句柄）。
fn snapshot_of(authorized: &AuthorizedTask, task_id: &str) -> TaskSnapshot {
    snapshot_with(
        task_id.to_string(),
        &authorized.owner,
        &authorized.client_instance,
        &authorized.task.snapshot(),
    )
}

/// 铸造任务 id：`shell-<完整 UUIDv7>`（禁止截断，源 issue 2026-08-05）。
pub fn mint_task_id() -> TaskId {
    format!("shell-{}", uuid::Uuid::now_v7())
}

/// 命令摘要（源：前 80 字符）。
pub fn summarize(command: &str) -> String {
    command.chars().take(COMMAND_SUMMARY_CHARS).collect()
}
