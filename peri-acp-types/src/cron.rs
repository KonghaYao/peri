//! Cron 契约（触发事件 + 调度器端口）。
//!
//! `CronTrigger` 自 `peri-middlewares/src/cron/mod.rs` 迁入（3.0 批 2 波 1）；
//! `CronSchedulerPort` 为装配注入端口（波 2）：宿主装配点构造具体
//! `CronScheduler` 后 upcast 注入，ACP 侧只持端口接口。middlewares 的
//! `CronScheduler` 实现该端口（`impl CronSchedulerPort for Mutex<CronScheduler>`）。

use std::any::Any;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

/// 单条 cron 触发提醒可承载的 prompt 预算（UTF-8 字节）。
///
/// 触发提醒的正文形如 `<goal-message>Cron task {task_id} triggered: {prompt}</goal-message>`，
/// 上限来自 canonical reminder 正文预算；这里再留出任务身份与信封余量，创建端与
/// 消费端共用同一常量，避免"创建放行、触发 panic"。
pub const MAX_CRON_PROMPT_BYTES: usize =
    crate::system_reminder::MAX_REMINDER_BODY_BYTES - MAX_CRON_PROMPT_ENVELOPE_BYTES;

/// 触发提醒信封（固定前缀/后缀 + task_id 与字段名的实际余量）预留字节。
const MAX_CRON_PROMPT_ENVELOPE_BYTES: usize = 4 * 1024;

/// 触发事件（由 CronScheduler 发送到 App）
#[derive(Debug, Clone)]
pub struct CronTrigger {
    pub task_id: String,
    /// 本次 firing 的稳定身份（调度器按计划触发时间派生）。
    ///
    /// 重试/重复投递复用同一身份，新的触发必须换新身份；不得用正文派生。
    pub firing_id: String,
    pub prompt: String,
}

/// cron prompt 超出可承载预算时的显式拒绝事实（不截断、不丢触发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CronPromptRejection {
    #[error("cron prompt is {actual} bytes; the limit is {MAX_CRON_PROMPT_BYTES} bytes")]
    PromptTooLong { actual: usize },
}

/// 触发提醒正文的唯一编码权威（创建端与消费端共用）。
pub fn cron_trigger_body(task_id: &str, prompt: &str) -> String {
    format!("<goal-message>Cron task {task_id} triggered: {prompt}</goal-message>")
}

const CRON_TRIGGER_BODY_PREFIX: &str = "<goal-message>Cron task ";
const CRON_TRIGGER_BODY_MIDDLE: &str = " triggered: ";
const CRON_TRIGGER_BODY_SUFFIX: &str = "</goal-message>";

/// 从 canonical 触发提醒里还原原始 prompt（[`cron_trigger_body`] 的逆映射）。
///
/// 返回 `None` 表示这不是一条可解释的 cron 触发提醒；调用方必须显式失败，
/// 不得用猜出的残缺指令继续执行。
pub fn cron_trigger_prompt(reminder: &crate::system_reminder::SystemReminder) -> Option<String> {
    let remainder = reminder
        .body
        .strip_prefix(CRON_TRIGGER_BODY_PREFIX)?
        .strip_suffix(CRON_TRIGGER_BODY_SUFFIX)?;
    let (_, prompt) = remainder.split_once(CRON_TRIGGER_BODY_MIDDLE)?;
    Some(prompt.to_owned())
}

/// 校验 prompt 是否在可承载预算内（创建端准入与消费端复核共用）。
pub fn validate_cron_prompt(prompt: &str) -> Result<(), CronPromptRejection> {
    if prompt.len() > MAX_CRON_PROMPT_BYTES {
        return Err(CronPromptRejection::PromptTooLong {
            actual: prompt.len(),
        });
    }
    Ok(())
}

/// 从 task + 本次 firing 身份派生稳定 delivery_id。
///
/// 同一 firing 的重复发布（重试、同一触发再次入队）复用同一身份而只投递一次；
/// 新的触发（新 `firing_id`）必然得到新身份，即使文案完全相同也各投递一次。
pub fn cron_firing_delivery_id(task_id: &str, firing_id: &str) -> crate::messages::MessageId {
    let mut hasher = Sha256::new();
    for part in ["peri-cron-trigger", task_id, firing_id] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    crate::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

/// cron 触发提醒构造失败的事实；两种原因都必须可观察，不得回退成截断指令。
#[derive(Debug, thiserror::Error)]
pub enum CronTriggerReminderError {
    #[error(transparent)]
    Prompt(#[from] CronPromptRejection),
    #[error("cron trigger reminder failed validation: {0}")]
    Invalid(#[from] crate::system_reminder::ReminderValidationError),
}

/// 构造 canonical cron 触发提醒（生产端唯一权威）。
///
/// - 正文承载完整 prompt（[`cron_trigger_body`]），超预算直接拒绝而不是截断；
/// - metadata 只放事件身份与长度摘要，不重复保存完整 prompt；
/// - 受众为 Model / Tui / Automation：Diagnostics 只接收诊断 DTO，不接收正文。
pub fn cron_trigger_reminder(
    task_id: &str,
    firing_id: &str,
    prompt: &str,
) -> Result<crate::system_reminder::TrustedSystemReminder, CronTriggerReminderError> {
    use crate::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    validate_cron_prompt(prompt)?;
    Ok(
        TrustedSystemReminderFactory::for_producer().construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("cron".into()),
            kind: "triggered".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![
                ReminderAudience::Model,
                ReminderAudience::Tui,
                ReminderAudience::Automation,
            ]),
            body: cron_trigger_body(task_id, prompt),
            summary: Some(format!("Cron task {task_id} triggered")),
            metadata: serde_json::json!({
                "task_id": task_id,
                "firing_id": firing_id,
                "prompt_bytes": prompt.len(),
            }),
        })?,
    )
}

/// Cron 任务信息（`CronScheduler::list_tasks` 的契约镜像，供 cron/list 命令面
/// 与 TUI 面板经 ACP 拿数据——契约层不引入 middlewares 的 `CronTask`）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CronTaskInfo {
    pub id: String,
    pub expression: String,
    /// 触发时提交的用户输入
    pub prompt: String,
    /// 下次触发时间（UTC）
    pub next_fire: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// Host 统一 continuation 入口接收的 cron/loop 触发。
///
/// `task_id` 必须原样保留到审批边界；`session_id` 在 session bridge 绑定，
/// 防止部署级 scheduler 的广播订阅丢失目标会话。
#[derive(Debug, Clone)]
pub struct CronContinuationRequest {
    pub session_id: String,
    pub recipient_control: crate::session_resources::ControlState,
    pub inbox: crate::session::MessageQueue,
    pub trigger: CronTrigger,
}

/// Cron 调度器端口（装配注入面，`peri-middlewares::cron::CronScheduler` 实现）。
///
/// ACP 侧只持 `Arc<dyn CronSchedulerPort>`；具体调度器（含注册/移除/时钟）
/// 由宿主装配点构造。订阅语义与 `CronScheduler::subscribe` 一致：
/// 返回的接收端收到每次触发的 clone。
pub trait CronSchedulerPort: Send + Sync {
    /// 订阅 cron 触发事件（每触发一次收到一条 `CronTrigger`）。
    fn subscribe(&self) -> mpsc::UnboundedReceiver<CronTrigger>;

    /// 全部任务快照（cron/list 命令面数据源；TUI 面板经 ACP 拿数据）。
    fn list_tasks(&self) -> Vec<CronTaskInfo>;

    /// 切换任务启用状态（返回是否命中）。
    fn toggle(&self, id: &str) -> bool;

    /// 移除任务（返回是否命中）。
    fn remove(&self, id: &str) -> bool;

    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;
}

impl dyn CronSchedulerPort {
    /// 将 `Arc<dyn CronSchedulerPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: CronSchedulerPort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn CronSchedulerPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，cron 工具注册的 scheduler 与 tick/bridge 订阅的
            // scheduler 分离，cron 触发完全静默（issue
            // 2026-08-07-cron-tool-task-never-triggers；同构
            // 2026-08-06-e2e-workflow-not-completing）。
            if (*ptr).as_any().type_id() == std::any::TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

#[cfg(test)]
#[path = "cron_test.rs"]
mod tests;
