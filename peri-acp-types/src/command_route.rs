//! 路由表条目契约（设计 `docs/design/command-system.md` §129/§148 与
//! Phase 1 计划步骤 6/7）。
//!
//! `RouteEntry` 是路由表的单一事实源：lexical（全名 / 别名 / args schema）、
//! handler（执行者引用）与 provenance（来源域 + 生命周期）三属性同置一条目，
//! **顶层扁平最终形态**（全局检查 P1-1 定案）——fullname 为唯一键，投影元数据
//! （kind / category / args_schema / level）直接落条目本体，不做嵌套；
//! 注册表持 `Arc<dyn CommandHandler>`，不 import 任何 handler 实现（设计 §72）。
//!
//! 来源域（[`CommandSource`]）对应词法保留域（设计 §44-59）：`core` / `ui` 为
//! 第一等级（可裸名 / 1 层显式），`mcp` / `plugin` / `user` 外部来源中——
//! plugin/user 为第二等级完整 2 层形态（namespace 首段由 provenance 声明，
//! 不可伪造，设计 §58）；**Mcp（决策 1）为外部动态域短形态 `{server}:{skill}`，
//! server 名即词法首段域**。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::command_args::ArgsSchema;
use super::command_handler::CommandHandler;
use super::command_name::CommandLevel;

/// 投影展示 kind（设计 §85：Command | Skill | McpSkill | Panel；注册时由 handler
/// 域推导一次，存 RouteEntry；serde snake_case 即 wire 形态，Phase 3 投影直接复用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEntryKind {
    /// core 域内置命令；plugin / user 域暂归此。
    Command,
    /// core 域本地 skill。
    Skill,
    /// mcp 域动态注入。
    McpSkill,
    /// ui 域（TUI 上送注册，设计 §67）。
    Panel,
}

/// 声明来源域（设计 §44-59；`CommandSource::Mcp` 于决策 1 后词法首段 =
/// server 名，见下）。
///
/// 第一等级（可裸名 / 1 层显式）：[`CommandSource::Core`]（内置命令 +
/// 本地 skill）、[`CommandSource::Ui`]（TUI 面板）。
///
/// 外部来源：插件 / 用户命令为第二等级完整形态（`plugin:name:xxx` /
/// `user:xxx:yyy`，必须完整 2 层形态）；**MCP server（决策 1）为外部动态域
/// 短形态 `{server}:{skill}`**——server 名即词法首段域，无 namespace 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    /// 内置命令 + 本地 skill（第一等级，可裸名）。
    Core,
    /// TUI 面板（第一等级，上送注册，设计 §67）。
    Ui,
    /// 外部 MCP server（决策 1：词法首段域 = server 名，命令形态
    /// `{server}:{skill}`）。语义收缩：plugin 提供的 server key 形如
    /// `plugin:p1:demosrv` 时，命令面存末段派生名 `demosrv`（词法 2 段
    /// 上限）——从命令面反查元数据面 registry 会 miss，插件归属不可从
    /// 命令名追溯，属既有取舍（skill_discovery.rs `mcp_namespace`）。
    Mcp { server: String },
    /// 插件（第二等级，`plugin:插件名:name`）。
    Plugin { name: String },
    /// 用户定义（第二等级，`user:xxx`）。
    User { name: String },
}

impl CommandSource {
    /// 词法域（core/ui/mcp/plugin/user）——路由与投影的域前缀。
    ///
    /// 决策 1 例外：`Mcp` 返回 server 名本身（非 `"mcp"`）——MCP server
    /// 名视作与 plugin 命名空间等同的领域，命令全名 = `{server}:{skill}`，
    /// server 名即词法首段域（注册表 provenance 域校验据此把关，设计 §58
    /// namespace 首段不可伪造）。
    pub fn domain(&self) -> &str {
        match self {
            CommandSource::Core => "core",
            CommandSource::Ui => "ui",
            CommandSource::Mcp { server } => server,
            CommandSource::Plugin { .. } => "plugin",
            CommandSource::User { .. } => "user",
        }
    }

    /// 第二等级的 namespace（Plugin/User 的来源域内标识，对应词法
    /// namespace 段）；Mcp（决策 1）与第一等级为 `None`——server 名已充当
    /// 词法首段域，无独立 namespace 段。
    pub fn namespace(&self) -> Option<&str> {
        match self {
            CommandSource::Mcp { .. } | CommandSource::Core | CommandSource::Ui => None,
            CommandSource::Plugin { name } => Some(name),
            CommandSource::User { name } => Some(name),
        }
    }

    /// 等级推导：core/ui → Level1；mcp/plugin/user → Level2（决策 1：Mcp
    /// 保持 Level2——完整全名展示、不登记裸名）。
    pub fn level(&self) -> CommandLevel {
        match self {
            CommandSource::Core | CommandSource::Ui => CommandLevel::Level1,
            CommandSource::Mcp { .. }
            | CommandSource::Plugin { .. }
            | CommandSource::User { .. } => CommandLevel::Level2,
        }
    }
}

/// 条目生命周期（设计 §148「已连接 / 发现中 / 已发现 / 断连清理」四态）。
///
/// 静态条目（core / ui）恒为 [`CommandLifecycle::Connected`]；动态条目
/// （mcp / plugin / user）随外部系统连接 → 发现 → 断连在四态间转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLifecycle {
    /// 静态条目（core / ui）恒为 Connected。
    Connected,
    /// 发现任务进行中（`Started → Discovered` 不占位注册，设计 §65）。
    Discovering,
    /// 发现完成（注册生效）。
    Discovered,
    /// 断连清理中（按 namespace 前缀批量注销）。
    Disconnecting,
}

/// 声明来源 + 生命周期（设计 §129/§148：条目携带 provenance）。
///
/// `source` 静态（声明时定，词法域 + 来源域内标识），`lifecycle` 动态
/// （注册表运行时更新）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProvenance {
    /// 来源域 + 来源域内标识（词法保留域，level 推导依据，设计 §58）。
    pub source: CommandSource,
    /// 条目生命周期（静态常驻 / 动态注入四态）。
    pub lifecycle: CommandLifecycle,
}

/// 路由表条目（设计 §129：lexical / handler / provenance 三属性，图 2 单一事实源；
/// **顶层扁平最终形态**——fullname 唯一键 + 投影元数据（kind / category /
/// args_schema / level）直接落条目本体；注册表持 `Arc<dyn CommandHandler>`，
/// 不 import 任何 handler 实现，设计 §72）。
///
/// Clone：handler 为 `Arc<dyn CommandHandler>`（Arc 克隆不要求 handler 实现
/// Clone），条目可整体克隆——装配面（Phase 6 B2/C1：插件条目预转后注入
/// session 管理器）与注册表 snapshot 复用。含 `Arc<dyn CommandHandler>`，
/// 不整体 derive Serialize——投影序列化由 Phase 3 按字段手工映射（元数据
/// 字段组的 serde 属性在投影 struct 上落位）。
#[derive(Clone)]
pub struct RouteEntry {
    /// 唯一键 = 全名小写（设计 §57/§86；注册路径生成，禁止裸名键）。
    pub fullname: String,
    /// 别名（小写登记）。
    pub aliases: Vec<String>,
    /// 投影描述。
    pub description: String,
    /// 投影 kind（注册时由 handler 域推导一次，设计 §85）。
    pub kind: CommandEntryKind,
    /// 自由文本分类（设计 §85 category?；None = 投影省略）。
    pub category: Option<String>,
    /// 参数 schema（设计 §73 完整 serde 模型；None = 投影省略）。
    pub args_schema: Option<ArgsSchema>,
    /// 执行者引用（trait object，不 import 具体实现，设计 §72）。
    pub handler: Arc<dyn CommandHandler>,
    /// 声明来源 + 生命周期（设计 §129/§148）。
    pub provenance: CommandProvenance,
}

impl RouteEntry {
    /// 等级推导（设计 §85：core/ui → Level1；mcp/plugin/user → Level2）。
    /// 不做冗余字段——单一推导点 = `CommandSource::level()`，杜绝同步漂移。
    pub fn level(&self) -> CommandLevel {
        self.provenance.source.level()
    }
}

/// TUI 上送注册的 ui 域命令明细（设计 §88：`ui_commands: bool →
/// `Vec<UiCommandSpec>` 的明细载体；wire：`peri.uiCommands` 数组元素）。
///
/// 已由 [`crate::peri_caps::PeriCaps`] 承载（门控反转：TUI 声明明细 → ACP
/// 注册为 `ui:*` 条目）；ACP 侧 `UI_COMMANDS` 常量已删除，ui 条目由发送侧
/// 挂载点（stdio/notify 初始化路径）注册进注册表，投影经注册表 snapshot 下发。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct UiCommandSpec {
    /// 命令名（注册为 `ui:<name>` 的 name 段）。
    pub name: String,
    /// 别名（小写登记）。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 命令描述。
    pub description: String,
    /// 参数 schema（None = 无参数，投影省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<ArgsSchema>,
}

#[cfg(test)]
#[path = "command_route_test.rs"]
mod tests;
