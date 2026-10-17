//! Goal 的周期恢复调度。
//!
//! 两类触发共用同一调度器：
//! - goal 进入 `Blocked`：公开状态机仍把 Blocked 视为终态；本模块是唯一的
//!   受控出口，每隔 [`super::DEFAULT_GOAL_RECOVERY_INTERVAL`] 把 goal 重新打开
//!   为 Active；
//! - goal 仍为 `Active`，但一轮以可恢复错误结束（模型/网络/compact/工具等）：
//!   下个周期投递恢复提醒。
//!
//! 两种情况都向会话 mailbox 投递一条 Required Defer，经既有 activation /
//! continuation 通路唤醒 agent。恢复轮次成功运行后由 GoalMiddleware 调用
//! [`super::GoalController::acknowledge_recovery`] 停止重试；若模型调用在唤醒
//! 前失败，下一周期会再次投递新消息（新 admission，不被失败水位抑制）。
//!
//! 生命周期：调度器随 `GoalState` 创建；`clear` / `complete` / 新 goal /
//! `session/cancel` / 会话关闭都会取消当前调度。

use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::RwLock;
use peri_acp_types::goal::{GoalStatus, GoalStore, ThreadGoal};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageQueue, MessageSource, QueuedMessage,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{GoalState, GoalStateInner};

/// 恢复调度器的固定配置（间隔由构造方注入，测试可缩短）。
#[derive(Clone)]
pub(super) struct RecoverySchedule {
    queue: MessageQueue,
    interval: Duration,
}

impl RecoverySchedule {
    pub(super) fn new(queue: MessageQueue, interval: Duration) -> Self {
        Self { queue, interval }
    }

    fn is_enabled(&self) -> bool {
        !self.interval.is_zero()
    }
}

/// 当前恢复周期的触发事实（决定提醒文案；不影响调度）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RecoveryTrigger {
    /// Agent 主动宣告 blocked；重开 goal 后尝试解除。
    Blocked { reason: String },
    /// 上一轮以可恢复错误结束（模型/网络/compact 等）；goal 仍为 Active。
    TurnError { reason: String },
}

impl RecoveryTrigger {
    fn reason(&self) -> &str {
        match self {
            Self::Blocked { reason } | Self::TurnError { reason } => reason,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Blocked { .. } => "blocked",
            Self::TurnError { .. } => "turn_error",
        }
    }
}

/// 当前 blocked 周期的恢复控制状态。
#[derive(Default)]
pub(super) struct RecoveryControl {
    generation: u64,
    cancel: Option<CancellationToken>,
    acknowledged: bool,
    attempts: u32,
}

impl GoalState {
    /// 为 blocked goal 启动（或重启）恢复调度。
    ///
    /// 调用方必须已把 goal 置为 `Blocked`。未配置恢复或间隔为 0 时 no-op。
    pub(super) fn arm_recovery(&self, goal_id: &str, trigger: RecoveryTrigger) {
        self.start_recovery(goal_id, trigger);
    }

    /// 一次终态错误结束了当前 turn，但 goal 仍为 Active：安排下一轮恢复尝试。
    ///
    /// - goal 非 Active / 不存在：no-op；
    /// - 已有恢复周期（例如 blocked 重试或前一次错误重试）：no-op，避免重置间隔；
    /// - 未配置恢复：no-op。
    pub(super) fn arm_error_recovery(&self, reason: &str) {
        if !self.recovery.as_ref().is_some_and(|s| s.is_enabled()) {
            return;
        }
        let goal_id = {
            let guard = self.inner.read();
            let Some(goal) = guard.goal.as_ref() else {
                return;
            };
            if goal.status != GoalStatus::Active || guard.recovery.cancel.is_some() {
                return;
            }
            goal.goal_id.clone()
        };
        // The read-lock check keeps duplicate error notifications from resetting
        // the interval; start_recovery re-reads state before delivering.
        self.start_recovery(
            &goal_id,
            RecoveryTrigger::TurnError {
                reason: reason.to_owned(),
            },
        );
    }

    fn start_recovery(&self, goal_id: &str, trigger: RecoveryTrigger) {
        let Some(schedule) = self.recovery.as_ref().filter(|s| s.is_enabled()) else {
            return;
        };
        let (token, generation) = {
            let mut guard = self.inner.write();
            if let Some(previous) = guard.recovery.cancel.take() {
                previous.cancel();
            }
            guard.recovery.generation = guard.recovery.generation.wrapping_add(1);
            guard.recovery.acknowledged = false;
            guard.recovery.attempts = 0;
            let token = CancellationToken::new();
            guard.recovery.cancel = Some(token.clone());
            (token, guard.recovery.generation)
        };

        let queue = schedule.queue.clone();
        let interval = schedule.interval;
        let inner = Arc::downgrade(&self.inner);
        let goal_id = goal_id.to_owned();
        tokio::spawn(async move {
            run_recovery(inner, queue, interval, goal_id, trigger, generation, token).await;
        });
    }

    /// 停止当前恢复调度（幂等）。会话关闭 / 用户取消 / goal 终态时调用。
    pub(crate) fn cancel_recovery(&self) {
        let mut guard = self.inner.write();
        guard.recovery.generation = guard.recovery.generation.wrapping_add(1);
        guard.recovery.acknowledged = false;
        guard.recovery.attempts = 0;
        if let Some(token) = guard.recovery.cancel.take() {
            token.cancel();
        }
    }

    /// 恢复轮次已成功运行：停止后续重试。
    pub(super) fn mark_recovery_acknowledged(&self) {
        let mut guard = self.inner.write();
        guard.recovery.acknowledged = true;
        if let Some(token) = guard.recovery.cancel.take() {
            token.cancel();
        }
    }
}

enum RecoveryStep {
    Retry {
        goal: ThreadGoal,
        thread_id: String,
        store: Arc<dyn GoalStore>,
        attempt: u32,
        reopened: bool,
    },
    Stop,
}

async fn run_recovery(
    inner: Weak<RwLock<GoalStateInner>>,
    queue: MessageQueue,
    interval: Duration,
    goal_id: String,
    trigger: RecoveryTrigger,
    generation: u64,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = peri_time::sleep(interval) => {}
        }

        let step = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let mut guard = inner.write();
            if guard.recovery.generation != generation || guard.recovery.cancel.is_none() {
                return;
            }
            let status = {
                let Some(goal) = guard.goal.as_ref() else {
                    return;
                };
                if goal.goal_id != goal_id {
                    return;
                }
                goal.status
            };

            match status {
                GoalStatus::Blocked => {
                    let goal = {
                        let goal = guard.goal.as_mut().expect("goal checked above");
                        if !goal.reopen_for_recovery(peri_time::now_wall()) {
                            return;
                        }
                        goal.clone()
                    };
                    guard.recovery.attempts = guard.recovery.attempts.saturating_add(1);
                    RecoveryStep::Retry {
                        goal,
                        thread_id: guard.thread_id.clone(),
                        store: guard.store.clone(),
                        attempt: guard.recovery.attempts,
                        reopened: true,
                    }
                }
                // A previous wake could not reach a completed agent turn (for
                // example the provider was still unavailable). Keep the goal
                // Active and deliver a fresh Required message each interval.
                GoalStatus::Active if !guard.recovery.acknowledged => {
                    let goal = guard.goal.as_ref().expect("goal checked above").clone();
                    guard.recovery.attempts = guard.recovery.attempts.saturating_add(1);
                    RecoveryStep::Retry {
                        goal,
                        thread_id: guard.thread_id.clone(),
                        store: guard.store.clone(),
                        attempt: guard.recovery.attempts,
                        reopened: false,
                    }
                }
                _ => RecoveryStep::Stop,
            }
        };

        match step {
            RecoveryStep::Stop => return,
            RecoveryStep::Retry {
                goal,
                thread_id,
                store,
                attempt,
                reopened,
            } => {
                if reopened {
                    if let Err(error) = store.save(&thread_id, goal.clone()).await {
                        tracing::warn!(
                            error = %error,
                            goal_id = %goal.goal_id,
                            "GoalState: 恢复重开后的 store 写入失败，保留内存状态"
                        );
                    }
                }
                if let Err(error) = push_recovery_reminder(&queue, &goal, &trigger, attempt) {
                    tracing::warn!(
                        error = %error,
                        goal_id = %goal.goal_id,
                        attempt,
                        "GoalState: 恢复提醒构造失败，下一周期重试"
                    );
                }
            }
        }
    }
}

fn push_recovery_reminder(
    queue: &MessageQueue,
    goal: &ThreadGoal,
    trigger: &RecoveryTrigger,
    attempt: u32,
) -> Result<(), String> {
    // The reason/objective originate from model or user text and can exceed the
    // canonical reminder budget; bound each field without splitting a char.
    let reason = bounded_field(trigger.reason());
    let objective = bounded_field(&goal.objective);
    let body = match trigger {
        RecoveryTrigger::Blocked { .. } => format!(
            "<goal-message>\n\
             [Goal Recovery Attempt {attempt}]\n\
             The goal was blocked: {reason}\n\
             Check whether the blocker is resolved now.\n\
             - Resolved → continue working toward: {objective}\n\
             - Still blocked → call goal(block, reason) again\n\
             </goal-message>"
        ),
        RecoveryTrigger::TurnError { .. } => format!(
            "<goal-message>\n\
             [Goal Recovery Attempt {attempt}]\n\
             The previous turn ended with an error: {reason}\n\
             The goal is still active. Retry the next step toward: {objective}\n\
             - If the blocker persists → call goal(block, reason)\n\
             </goal-message>"
        ),
    };
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Guidance,
        source: ReminderSource("goal".into()),
        kind: "recovery".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Automation]),
        body,
        summary: Some(format!("Goal recovery attempt {attempt}")),
        metadata: json!({
            "goal_id": goal.goal_id,
            "recovery_attempt": attempt,
            "trigger": trigger.kind(),
            "trigger_reason": reason,
        }),
    };
    let trusted = TrustedSystemReminderFactory::for_recovery()
        .construct(reminder)
        .map_err(|error| error.to_string())?;
    queue.push(
        QueuedMessage::system_reminder(MessageKind::Defer, MessageSource::GoalSteering, trusted)
            .with_policy(MessagePolicy::ensure_processing()),
    );
    Ok(())
}

/// 将恢复提醒中的自由文本限制在 canonical reminder 预算内（按字符边界）。
fn bounded_field(value: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let mut chars = value.chars();
    let bounded: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}
