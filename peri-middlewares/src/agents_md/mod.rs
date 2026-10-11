//! AgentsMdMiddleware — 项目指令（AGENTS.md / CLAUDE.md / CLAUDE.local.md）
//! 的**纯贡献 adapter**（W5，plan §6.1/§6.3）。
//!
//! 迁移事实（E14/E15 → plan §6.3「删除全部读盘/搜索/import 行为」）：
//!
//! - **不再读盘**：正文由会话创建期的内容准入（P4）从 builtin `workspace`
//!   实例的 `peri-instruction://workspace/{main|local}` 资源读取（provider 负责
//!   候选优先级、`@import` 深度 3 展开与环防护、越界拒绝），随冻结数据注入
//!   （`FrozenInstructions` → `with_frozen_parts`）；
//! - 本中间件只保留「贡献字符串的持有与注入」：`prompt_contribution()` 返回
//!   main + 空行 + local 的合成文本；`before_agent` 是无副作用空 hook；
//! - 找不到内容（无冻结正文 / 指令面被关闭 / 未装配）⇒ 不贡献，
//!   **不回落磁盘**（X4/J5：任何组件不得回落）；
//! - `excludes` 语义已随候选选择整体归 provider 输入
//!   （`WorkspaceResourcesInput::instruction_excludes`），本中间件不再持有它。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    middleware::{capabilities as hook_state, r#trait::Middleware},
};

/// AgentsMdMiddleware - 注入项目指引文件（AGENTS.md / CLAUDE.md）
///
/// 内容来源是会话冻结的项目指令快照（见模块文档）；本类型不执行文件系统 I/O。
pub struct AgentsMdMiddleware {
    /// 贡献文本（构造时由冻结内容合成；只读使用）。
    cached_contribution: Arc<RwLock<Option<String>>>,
}

impl AgentsMdMiddleware {
    pub fn new() -> Self {
        Self {
            cached_contribution: Arc::new(RwLock::new(None)),
        }
    }

    /// 注入冻结的指令正文（main 已含 `@import` 展开；local 为 `CLAUDE.local.md`）。
    ///
    /// M4：main 与 local 是**独立输入**，任一非空都应贡献——`None`（该来源
    /// 不可得）与 `Some("")`（显式空快照）语义不同但都不贡献，且**不授权重新
    /// 扫描**（本中间件无任何文件系统回落）。合成口径：非空白部分按
    /// main → local 顺序以空行连接；全为空白时整体不贡献。
    pub fn with_frozen_parts(self, main: Option<String>, local: Option<String>) -> Self {
        let mut parts: Vec<String> = Vec::new();
        for part in [main, local].into_iter().flatten() {
            if !part.trim().is_empty() {
                parts.push(part);
            }
        }
        if !parts.is_empty() {
            *self.cached_contribution.write().unwrap() = Some(parts.join("\n\n"));
        }
        self
    }
}

impl Default for AgentsMdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentsMdMiddleware {
    fn name(&self) -> &str {
        "AgentsMdMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }

    async fn before_agent(&self, _state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        // W5：本中间件已无读盘/搜索/import 行为——贡献在构造时由冻结内容定格，
        // 会话内不再变化（ARC-FROZEN-001）。保留空 hook 以维持链槽位与
        // `name()` 判定（MetaHarness 关闭键）不变。
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
