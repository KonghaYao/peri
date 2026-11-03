//! 进入会话时的恢复（设计 §3/§4）：根范围、实例判定、提醒接纳与延后启动。
//!
//! 模块边界：
//!
//! - [`scope`]：范围切分的纯函数（根 / child / 直接子候选）；
//! - [`reminder`]：可信恢复提醒的正文、接纳形状与 MQ 去重；
//! - [`parent_notice`]：冷 child 恢复 turn 结束后投递给父 thread 的结果通知；
//! - [`coordinator`]：`SessionRecovery` 实现、single-flight 与启动门。
//!
//! 恢复不建表、不写工作意图；历史展示与元数据查询都不经过本模块。

mod coordinator;
mod parent_notice;
mod reminder;
mod scope;

pub(crate) use coordinator::HostSessionRecovery;
pub(crate) use parent_notice::{deliver_child_turn, turn_outcome};
