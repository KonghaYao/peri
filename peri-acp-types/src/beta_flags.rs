//! Beta flag 注册表与投影/冻结值契约。
//!
//! 设计单一事实源：`docs/design/beta-flags.md`。flag **存在性**的唯一权威是本模块
//! 的 [`BETA_FLAGS`]（与 MetaHarness 常量表同址，集中声明）：settings 校验、装配方
//! （`peri-middlewares` / `peri-mcp-workspace`）与 TUI 都从本表取值，消费方**不写
//! 字符串字面量**。
//!
//! 边界（设计 §定位）：
//! - flag 不是通用配置：常态能力参数用各自领域的 typed 字段表达；
//! - flag 不是 MetaHarness：MetaHarness 关闭已默认装配的组件，flag 开启未默认启用
//!   的能力；两者都服从配置权威面，注册表与消费面独立；
//! - flag 不承载安全与数据兼容语义，也不做会话内热生效。
//!
//! 所有条目默认值**恒为 false**，注册表不得声明默认开启条目。
//!
//! [`BetaFlags`] 是 resolve 投影（`覆盖.get(id).unwrap_or(false)`）的载体：由合并后
//! settings 派生（`peri-config`），会话装配期读取一次后随冻结载体
//! （`peri_agent::session::FrozenContext`）传播——执行路径不重读配置、不解析 flag
//! 语义。

use std::collections::BTreeMap;

/// 注册表条目：稳定 id 与 canonical 描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BetaFlag {
    /// 稳定 id（kebab-case）。一经发布不得改名（settings 键 / 日志 / 面板身份），
    /// 改名等同于「废除 + 引入」。
    pub id: &'static str,
    /// canonical 描述：面板 i18n key `beta-desc-<id>` 缺失时的回退文本。
    pub description: &'static str,
}

/// `full-async-tools`：`Bash` 与 `Agent` 在调用未显式给出 `run_in_background` 时
/// 缺省走后台（显式 `false` 仍前台）。
///
/// 消费方引用本常量，不写 `"full-async-tools"` 字面量。
pub const FULL_ASYNC_TOOLS: &str = "full-async-tools";

/// Beta flag 清单（顺序即 TUI 面板区块顺序，发布后保持稳定）。
pub const BETA_FLAGS: &[BetaFlag] = &[BetaFlag {
    id: FULL_ASYNC_TOOLS,
    description:
        "Bash and Agent default to background execution when the call omits run_in_background",
}];

/// 按 id 查注册表条目；未命中返回 `None`（未知 id 不是错误，按未覆盖处理）。
pub fn find(id: &str) -> Option<&'static BetaFlag> {
    BETA_FLAGS.iter().find(|flag| flag.id == id)
}

/// 覆盖来源层（诊断用；`explain` 与装配期日志的事实源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BetaFlagOrigin {
    /// 值来自 global settings（含 global 独有键）。
    Global,
    /// 值来自 workspace settings（workspace 覆盖胜出）。
    Workspace,
}

/// 单个已覆盖 flag 的有效值与其来源层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BetaFlagValue {
    pub enabled: bool,
    pub origin: BetaFlagOrigin,
}

/// flag 有效值投影：`覆盖.get(id).unwrap_or(false)`。
///
/// 只承载**已覆盖**的已知 id；未覆盖与未知 id 一律按 false 处理（[`Self::is_enabled`]），
/// 因此「快照缺失」或「配置面不可用」都不会意外开启能力。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BetaFlags {
    overrides: BTreeMap<String, BetaFlagValue>,
}

impl BetaFlags {
    /// 从已覆盖条目构造（键为已由注册表校验过的 flag id）。
    pub fn from_values(entries: impl IntoIterator<Item = (String, BetaFlagValue)>) -> Self {
        Self {
            overrides: entries.into_iter().collect(),
        }
    }

    /// 有效值：未覆盖、未知 id 与空投影一律 false。
    pub fn is_enabled(&self, id: &str) -> bool {
        self.overrides.get(id).is_some_and(|value| value.enabled)
    }

    /// 已覆盖条目的值（未覆盖返回 `None`，与「显式 false」区分）。
    pub fn value(&self, id: &str) -> Option<BetaFlagValue> {
        self.overrides.get(id).copied()
    }

    /// 已覆盖条目（含值）的只读快照，按 id 稳定顺序（持久化编码与诊断消费）。
    pub fn overrides(&self) -> impl Iterator<Item = (&str, BetaFlagValue)> {
        self.overrides
            .iter()
            .map(|(id, value)| (id.as_str(), *value))
    }

    /// 生效条目（`enabled == true`）的 id 与来源层，按 id 稳定顺序。
    pub fn enabled(&self) -> impl Iterator<Item = (&str, BetaFlagOrigin)> {
        self.overrides
            .iter()
            .filter(|(_, value)| value.enabled)
            .map(|(id, value)| (id.as_str(), value.origin))
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

#[cfg(test)]
#[path = "beta_flags_test.rs"]
mod tests;
