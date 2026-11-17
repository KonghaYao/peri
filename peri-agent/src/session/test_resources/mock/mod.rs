//! `#[cfg(test)]` 内存门面替身：只实现测试真正观察的行为，其余行为直接 panic。
//!
//! 用它而不是造假：`unimplemented` 的条目一旦被某个测试用到就会立刻失败，不会把
//! 「没覆盖」伪装成「通过」。需要真实绑定/事务语义的测试请用
//! [`TestSession`](super::test_resources::TestSession)（真实 `SessionResourcesImpl`）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    ChildResumeClaim, NewSessionMeta, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta, WorkState};
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};

pub(crate) use mock_session_resources::ResumeLoadGate;

use mock_session_resources::default_meta_for;

use mock_session_resources::child_meta;

pub(crate) use mock_session_resources::MockSessionResources;

use mock_session_resources::MockResumeClaim;

// ── 子模块（按职责拆分；内部细节见各自文件头）────────────────────────────
/// 夹具便利方法：镜像迁移前 `ThreadStore` 的常用测试调用形态。
mod fixtures;
/// `peri_model::Model` 形态的假模型助手（生产 bridge 装配的测试入口）。
pub(crate) mod model;
/// 故障注入与观察入口（只覆盖被测试的行为）。
mod observe;
/// `SessionResources` 门面替身：逐个方法实现契约语义。
mod session_resources;

#[path = "mock_session_resources.rs"]
mod mock_session_resources;
