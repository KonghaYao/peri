//! Session 层契约类型（自 peri-agent 迁入；`peri-agent::session::{queue,turn,runtime}`
//! 与 `peri-agent::agent::session::{inbox,cron_owner}` 保留 re-export 保兼容）。
//!
//! 归位说明（§0 兜底：接口契约归 peri-acp-types）：MQ 消息管理、turn 身份、
//! inbox 唤醒、cron 触发桥、Agent 运行时注册表条目是跨层接口契约——
//! Agent 层持有实现与执行权，ACP / middlewares 只依赖本层契约类型。

mod cron_owner;
mod execution;
mod in_memory_mq_store;
mod inbox;
mod mq;
use in_memory_mq_store as mq_memory;
mod message_queue;
#[cfg(test)]
mod mq_test;
use message_queue as queue;
mod message_policy;
use message_policy as queue_policy;
mod agent_runtime;
#[cfg(test)]
mod queue_policy_test;
use agent_runtime as runtime;
mod user_input;

pub use cron_owner::CronOwner;
pub use execution::{
    bounded_error_message, ExecutionFailure, ExecutionFailureKind, PromptResult,
    TurnTelemetryOutcome, EXECUTION_FAILURE_FALLBACK_MESSAGE,
};
pub use inbox::{InboxHandle, SessionInbox};
#[cfg(feature = "test-support")]
pub use mq::ReceiveGate;
pub use mq::{
    MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter, MqHeader, MqInput,
    MqReceiveDisposition, MqReceiveItem, MqReceiveItemOutcome, MqReceiveOutcome, MqReceivePurpose,
    MqReceiveSelection, MqReceiveStatus, MqSendItemOutcome, MqSendOutcome, MqSendStatus,
    MqWatermark, RetainedReminder, SessionResourcesMqStore, ThreadMqStore,
};
#[doc(hidden)]
pub use mq_memory::InMemoryMqStore;
pub use queue::{
    MessageKind, MessageQueue, MessageSource, ProducerQueue, QueueClosed, QueueOwner,
    QueuedMessage, QueuedPayload, ReceivePermit,
};
pub use queue_policy::{
    ExecutionBinding, MessageActivation, MessageDisposition, MessagePolicy, MessageRequirement,
};
pub use runtime::{
    cancel_all_agents, cancel_all_in, cancel_cascade_agents, cancel_cascade_agents_collecting,
    cancel_cascade_in, AgentRuntime,
};
pub use user_input::{
    DispatchUserInputsRequest, EnqueueUserInputRequest, PluginSelection, TakeBackUserInputRequest,
    UserInput, UserInputItemResult, UserInputQueueItem, UserInputQueueReceipt,
    UserInputQueueSnapshot, UserInputQueueSnapshotRequest, UserInputQueueTerminalItem,
    UserInputState,
};

mod turn_id;
pub use turn_id::TurnId;

mod session_access_port;
pub use session_access_port::SessionAccessPort;

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
