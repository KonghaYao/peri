//! Shared frozen-data construction for session/new.
//!
//! Both TUI and Stdio paths build identical frozen data at session creation.
//! 实际构造已内聚为 [`crate::session::executor::FrozenSessionData::build`]，
//! 此模块仅保留薄包装，维持既有 import 路径稳定。
//!
//! v2 迁移：构造同时产出 `peri_agent::session::FrozenContext`，
//! 可通过 `FrozenSessionData::v2_frozen()` 访问。

use std::path::PathBuf;

use peri_middlewares::skills::SkillRoot;

use crate::session::executor::FrozenSessionData;

/// Build frozen session data from the given parameters.
///
/// 委托给 [`FrozenSessionData::build`]（Immutable Value Object 的唯一构造入口）。
/// 保留此自由函数以兼容既有调用点；新代码请优先使用关联函数。
pub fn build_frozen_session_data(
    cwd: &str,
    language: Option<&str>,
    plugin_skill_roots: &[SkillRoot],
    plugin_agent_dirs: &[PathBuf],
    frozen_date: &str,
) -> FrozenSessionData {
    FrozenSessionData::build(
        cwd,
        language,
        plugin_skill_roots,
        plugin_agent_dirs,
        frozen_date,
    )
}
