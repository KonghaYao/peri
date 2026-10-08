//! 插件来源准入（M6）：关闭位在读取任何插件来源之前判定，是唯一入口。
//!
//! 由 `loader.rs` 迁出（STD-SIZE-001）；`plugin/mod.rs` 保留 re-export。

use std::collections::HashMap;
use std::path::Path;

use super::loader::{load_enabled_plugins_aggregated_readonly, load_enabled_plugins_for_mcp};
use super::loader::{LoadedPlugin, LoaderError, PluginLoadResult};

/// 插件来源准入（M6）：**唯一闭合位**，必须在读取任何插件来源之前判定。
///
/// 语义：`PluginMiddleware ∈ 冻结/session-local disabled_middlewares` ⇒ 关闭。
/// 关闭时插件 skills / agents / commands / hooks / MCP 与子 Agent 继承面一律
/// 不参与装配（关闭语义要求这些能力不存在，而不是先读进来再藏起来）。
///
/// 本类型是**唯一**入口：装配面不自行拼 `PluginMiddleware` 字面量、不按插件名
/// 过滤、也不在关闭后回退到另一条加载路径（`load_enabled_plugins` 等裸入口不
/// 带策略，只允许测试与不含会话策略的调用点使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSourceAdmission {
    Open,
    Closed,
}

impl PluginSourceAdmission {
    /// 从冻结/session-local 的关闭集派生（与链槽装配的跳过判据同一字面量）。
    pub fn from_disabled(disabled: &std::collections::HashSet<String>) -> Self {
        if crate::assembly::plugin_face_closed(disabled) {
            Self::Closed
        } else {
            Self::Open
        }
    }

    /// 从 `meta_harness` 配置投影派生（会话准备期在构建 frozen 之前使用；
    /// 与 `build_meta_harness_state` 的 `middleware + false` 规则同源）。
    pub fn from_meta_harness(config: Option<&HashMap<String, bool>>) -> Self {
        let closed = config
            .and_then(|map| map.get(crate::assembly::PLUGIN_FACE_CLOSED_KEY))
            .is_some_and(|enabled| !enabled);
        if closed {
            Self::Closed
        } else {
            Self::Open
        }
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }

    /// 从已派生的布尔位构造（pool 在装配期一次派生后注入；避免消费点各写判据）。
    pub fn from_closed(closed: bool) -> Self {
        if closed {
            Self::Closed
        } else {
            Self::Open
        }
    }

    /// 会话准备（严格只读）聚合：`Closed` ⇒ `Ok(None)` 且**不读插件目录**
    /// （不产生清单解析副作用，也不留下可被照抄的候选内容）。
    pub fn load_aggregated_readonly(
        self,
        claude_dir: &Path,
        cwd: Option<&Path>,
    ) -> Result<Option<PluginLoadResult>, LoaderError> {
        if self.is_closed() {
            return Ok(None);
        }
        load_enabled_plugins_aggregated_readonly(claude_dir, cwd).map(Some)
    }

    /// MCP 合并的插件输入：`Closed` ⇒ 空列表且**不读插件目录**（严格 MCP 路径
    /// 的失败也不会被触发——关闭的会话不因插件 MCP 配置而启动失败）。
    pub fn load_for_mcp(
        self,
        claude_dir: &Path,
        cwd: Option<&Path>,
    ) -> Result<Vec<LoadedPlugin>, LoaderError> {
        if self.is_closed() {
            return Ok(Vec::new());
        }
        load_enabled_plugins_for_mcp(claude_dir, cwd)
    }
}
