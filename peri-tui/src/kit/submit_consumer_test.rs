//! Tests for submit_consumer
//!
//! 按职责拆分：`support` 提供共享夹具，其余文件分别覆盖 FIFO、会话切换、
//! 车道原语、命令与消费者生命周期。

#[path = "submit_consumer_test/support.rs"]
mod support;

#[path = "submit_consumer_test/fifo.rs"]
mod fifo;

#[path = "submit_consumer_test/session_switch.rs"]
mod session_switch;

#[path = "submit_consumer_test/lane.rs"]
mod lane;

#[path = "submit_consumer_test/commands.rs"]
mod commands;

#[path = "submit_consumer_test/lifecycle.rs"]
mod lifecycle;
