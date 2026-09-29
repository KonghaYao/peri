//! Git ref 采样与 `workspace://git/ref` 资源正文（服务端侧）。
//!
//! 来源：宿主 `peri-middlewares::git_watch`（本批下沉删除）的**逐字搬迁**——触发门、
//! 单飞、60s 节流（按采样**完成**时刻计时；采样失败不推进）、`NotRepository` 首次短路后
//! 不再 spawn、1s 超时、`GIT_OPTIONAL_LOCKS=0` 与 notice 文案都是行为契约（计划 §1
//! 「逐字保持项」）。差异只有两处（计划 D-3）：`kill_on_drop(true)`（旧实现超时后子进程
//! 成孤儿）与「服务端 call_tool 成功返回后触发」的新触发点（旧触发点是宿主 after_tool）。
//!
//! 资源正文（D-2）= 最近一次采样结论：有变化 → [`notice_text`]（旧
//! `info_message_if_changed` 逐字）；无变化 → [`snapshot_text`]；未采样 →
//! [`UNSAMPLED_RESOURCE_TEXT`]。提醒元数据由宿主内置（D-5），本模块不产出提醒。
//!
//! 「无订阅者 ⇒ 不采样（零 git 调用）」由调用方（`workspace.rs` 的 `spawn_git_sample`）
//! 保证：本模块只提供状态机，不自行判断订阅者。

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tokio::process::Command;

/// `workspace` 实例的 git ref 资源 URI（订阅 filter 与通知的唯一标识）。
pub const GIT_REF_RESOURCE_URI: &str = "workspace://git/ref";

/// 采样超时：与旧 `GIT_WATCH_SAMPLE_TIMEOUT` 逐字一致（1s）。
pub const GIT_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);

/// 采样节流窗口：与旧 `GIT_WATCH_THROTTLE` 逐字一致（60s，按完成时刻计时）。
pub const GIT_THROTTLE: Duration = Duration::from_secs(60);

/// 尚未采样时的资源正文（新文案，无旧对应；仅在「订阅已建立但还没有任何采样结论」时
/// 被 `resources/read` 读到）。
pub const UNSAMPLED_RESOURCE_TEXT: &str = "[Git watch] Repository ref has not been sampled yet.";

/// 监视快照：仅分支 + HEAD（不跟踪 working tree，旧实现同口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSnapshot {
    pub branch: String,
    pub head: String,
}

/// 采样结果（异步 git 子进程解析后；旧 `SampleOutcome` 逐字）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleOutcome {
    Repository(GitSnapshot),
    NotRepository,
    Failed,
}

/// 与上次快照比较，生成变化 notice；无变化或尚无基线时返回 `None`。
///
/// **逐字**搬迁自旧 `info_message_if_changed`：本函数的返回值在变化时就是提醒正文
/// （resource body = reminder body），文案不得改写。
pub fn notice_text(previous: Option<&GitSnapshot>, current: &GitSnapshot) -> Option<String> {
    let prev = previous?;
    if prev == current {
        return None;
    }

    let mut lines = vec!["[Git watch] Repository ref changed since the last sample:".to_string()];

    if prev.branch != current.branch {
        lines.push(format!("- Branch: {} → {}", prev.branch, current.branch));
    }
    if prev.head != current.head {
        lines.push(format!(
            "- HEAD: {} → {}",
            short_hash(&prev.head),
            short_hash(&current.head)
        ));
    }

    lines.push(String::new());
    lines.push(
        "Sampled after a tool run or turn start. Run `git status` and `git log -1` before irreversible git operations.".to_string(),
    );

    Some(lines.join("\n"))
}

/// 无变化时的资源正文（新增）：最近一次采样结论的快照读数。
///
/// 与 [`notice_text`] 同风格（`[Git watch]` 前缀 + 逐行读数 + 行动指引），但只描述
/// 当前值、不含箭头；采样触发口径按新事实（工具运行后）。
pub fn snapshot_text(snapshot: &GitSnapshot) -> String {
    format!(
        "[Git watch] Repository ref snapshot:\n- Branch: {}\n- HEAD: {}\n\nSampled after a tool run. Run `git status` and `git log -1` before irreversible git operations.",
        snapshot.branch,
        short_hash(&snapshot.head)
    )
}

/// 短 hash（7 位；不足 7 位原样；旧 `short_hash` 逐字）。
pub fn short_hash(full: &str) -> String {
    let trimmed = full.trim();
    if trimmed.len() <= 7 {
        trimmed.to_string()
    } else {
        trimmed[..7].to_string()
    }
}

/// 解析 `git rev-parse --is-inside-work-tree` + HEAD + branch 的合并输出（旧实现逐字）。
pub fn parse_sample_stdout(stdout: &str) -> SampleOutcome {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(work_tree) = lines.next() else {
        return SampleOutcome::Failed;
    };
    if work_tree != "true" {
        return SampleOutcome::NotRepository;
    }
    let Some(head) = lines.next() else {
        return SampleOutcome::Failed;
    };
    let Some(branch) = lines.next() else {
        return SampleOutcome::Failed;
    };
    SampleOutcome::Repository(GitSnapshot {
        branch: branch.to_string(),
        head: head.to_string(),
    })
}

/// 采样一次 git ref（旧 `run_git_sample` 逐字，唯一差异 = `kill_on_drop(true)`，D-3）。
pub async fn run_git_sample(cwd: &str) -> SampleOutcome {
    let output = match Command::new("git")
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true)
        .args([
            "rev-parse",
            "--is-inside-work-tree",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return SampleOutcome::Failed,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let work_tree = lines.next().unwrap_or("").trim();
    if work_tree != "true" {
        if work_tree == "false" {
            return SampleOutcome::NotRepository;
        }
        return SampleOutcome::Failed;
    }
    let head = lines.next().unwrap_or("").trim();
    let branch = lines.next().unwrap_or("").trim();
    if head.is_empty() || branch.is_empty() {
        return SampleOutcome::Failed;
    }

    parse_sample_stdout(&format!("true\n{head}\n{branch}\n"))
}

/// 仓库态三分类（旧 `RepoMode` 逐字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoMode {
    Unknown,
    NotRepository,
    Repository,
}

/// git ref 采样状态机（旧 `GitWatchInner` 的搬迁；调用方在 `call_tool` 成功后驱动）。
///
/// 并发语义（旧实现逐字）：
/// - `begin_sample` 先做 `NotRepository` 短路，再做单飞 CAS，最后按**完成**时刻节流；
/// - `finish_sample` 在 `Repository` 分支更新快照与资源正文、推进节流时刻；`Failed`
///   与 `NotRepository` 都不推进节流（失败不推进 = 下个触发点可立即重试）。
#[derive(Debug)]
pub struct GitWatchState {
    repo_mode: Mutex<RepoMode>,
    last_snapshot: Mutex<Option<GitSnapshot>>,
    /// 资源正文（最近一次采样结论；未采样 = [`UNSAMPLED_RESOURCE_TEXT`]）。
    resource_text: Mutex<String>,
    /// 上次采样**完成**时刻（60s 节流基准）。
    last_sample_completed_at: Mutex<Option<Instant>>,
    in_flight: AtomicBool,
    throttle: Duration,
    sample_timeout: Duration,
}

impl Default for GitWatchState {
    fn default() -> Self {
        Self::new()
    }
}

impl GitWatchState {
    /// 生产构造：60s 节流 + 1s 超时。
    pub fn new() -> Self {
        Self::with_timing(GIT_THROTTLE, GIT_SAMPLE_TIMEOUT)
    }

    /// 注入节流窗口与超时（**测试 seam**：生产恒为 [`GIT_THROTTLE`] / [`GIT_SAMPLE_TIMEOUT`]）。
    pub fn with_timing(throttle: Duration, sample_timeout: Duration) -> Self {
        Self {
            repo_mode: Mutex::new(RepoMode::Unknown),
            last_snapshot: Mutex::new(None),
            resource_text: Mutex::new(UNSAMPLED_RESOURCE_TEXT.to_string()),
            last_sample_completed_at: Mutex::new(None),
            in_flight: AtomicBool::new(false),
            throttle,
            sample_timeout,
        }
    }

    /// 采样超时（调用方的 `tokio::time::timeout` 取它，保持「超时不推进节流」在同一处收敛）。
    pub fn sample_timeout(&self) -> Duration {
        self.sample_timeout
    }

    /// 触发门（旧 `schedule_sample` 的判定序列逐字）：`true` = 本次由调用方 spawn 采样。
    ///
    /// 顺序不可调换：`NotRepository` 短路在最前（短路后不再 spawn，一次都不再 spawn）、
    /// 单飞 `in_flight`、完成时刻节流、CAS 抢占。
    pub fn begin_sample(&self) -> bool {
        if matches!(*self.repo_mode.lock(), RepoMode::NotRepository) {
            return false;
        }
        if self.in_flight.load(Ordering::Acquire) {
            return false;
        }
        if let Some(completed) = *self.last_sample_completed_at.lock() {
            if completed.elapsed() < self.throttle {
                return false;
            }
        }
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        true
    }

    /// 采样结束收口：更新仓库态 / 快照 / 资源正文，返回「需要通知订阅者」的 notice。
    ///
    /// 返回 `Some(notice)` 仅当**变化**（首采样与无变化都是 `None` ⇒ 不通知）；调用方
    /// 随后逐 sink 推送 `notifications/resources/updated`。
    pub fn finish_sample(&self, outcome: SampleOutcome) -> Option<String> {
        let notify = match outcome {
            SampleOutcome::Failed => None,
            SampleOutcome::NotRepository => {
                *self.repo_mode.lock() = RepoMode::NotRepository;
                None
            }
            SampleOutcome::Repository(current) => {
                *self.repo_mode.lock() = RepoMode::Repository;

                let notice = {
                    let mut snap_guard = self.last_snapshot.lock();
                    let previous = snap_guard.clone();
                    let notice = notice_text(previous.as_ref(), &current);
                    *snap_guard = Some(current.clone());
                    notice
                };

                // D-2：资源正文 = 最近一次采样结论（变化 → notice 逐字；无变化 → 快照文本）。
                *self.resource_text.lock() =
                    notice.clone().unwrap_or_else(|| snapshot_text(&current));

                // 节流按**完成**时刻计时（仅成功采样推进）。
                *self.last_sample_completed_at.lock() = Some(Instant::now());
                notice
            }
        };
        self.in_flight.store(false, Ordering::Release);
        notify
    }

    /// 当前资源正文（`resources/read` 命中时返回它）。
    pub fn resource_text(&self) -> String {
        self.resource_text.lock().clone()
    }
}

#[cfg(test)]
#[path = "git_watch_test.rs"]
mod tests;
