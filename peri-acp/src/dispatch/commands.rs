//! Build ACP available commands list, shared by TUI and stdio transports.
//!
//! Phase 3 起投影数据源切换为注册表（`CommandRegistry::snapshot()`）——
//! 命令元数据只在 RouteEntry 定义一次（设计不变式 1），本模块不再自造任何
//! 硬编码命令表（`UI_COMMANDS` 常量与 `build_available_commands` 已删除）。

use std::sync::Arc;

use agent_client_protocol_schema::v1::{AvailableCommand, AvailableCommandsUpdate};
use async_trait::async_trait;
use peri_acp_types::command::command_handler::{CommandHandler, CommandOutcome};
use peri_acp_types::command::command_name::CommandLevel;
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use peri_acp_types::command::CommandContext;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::PeriCaps;

/// ui 域占位 handler（门控反转核心，Phase 3 步骤 4）：Delegate 回 TUI——
/// Phase 4 落 TUI 本地拦截；此前 TUI 本地拦截 `/help` 等（submit_request.rs），
/// host 侧无调用路径。携带委托目标 `ui:<name>`。
#[derive(Clone)]
pub(crate) struct UiDelegatePlaceholder(pub(crate) String);

#[async_trait]
impl CommandHandler for UiDelegatePlaceholder {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Delegate(self.0.clone())
    }
}

/// caps.ui_commands 明细 → ui 域 RouteEntry（门控反转核心，Phase 3 步骤 4）：
/// fullname = `ui:<name>`（name 小写，唯一键）；kind = Panel；category = "ui"；
/// handler = [`UiDelegatePlaceholder`]（Delegate 回 TUI）。注册冲突由注册表
/// 纯拒绝裁决（第一等级裸名跨域互斥，如 ui:clear 与 core:clear），本函数
/// 不做过滤——全部明细构造，调用点注册时逐条裁决。
pub(crate) fn ui_route_entries(caps: &PeriCaps) -> Vec<RouteEntry> {
    caps.ui_commands
        .iter()
        .map(|spec| {
            let fullname = format!("ui:{}", spec.name.to_lowercase());
            RouteEntry {
                fullname: fullname.clone(),
                aliases: spec.aliases.clone(),
                description: spec.description.clone(),
                kind: CommandEntryKind::Panel,
                category: Some("ui".into()),
                args_schema: spec.args.clone(),
                handler: Arc::new(UiDelegatePlaceholder(fullname)),
                provenance: CommandProvenance {
                    source: CommandSource::Ui,
                    lifecycle: CommandLifecycle::Connected,
                },
            }
        })
        .collect()
}

/// ui 域注册（门控反转核心，Phase 3 步骤 4）：`caps.ui_commands` 明细注册进
/// 注册表（冲突 → 纯拒绝 + warn，不覆盖、不静默）。
///
/// **一次性注册契约**：仅发送侧挂载点（stdio `send_available_commands` /
/// notify `send_available_commands_update`）在注册表 `set_on_change` 挂载
/// **之前**调用——时序标注（防双发）：注册动作自身触发 on_change 一次，
/// 先挂载回调则同一次注册会再触发一次。on_change 回调路径（mcp 对账驱动
/// 投影重建）**不得**调用本函数。
///
/// 幂等：同 fullname 已存在视为已注册，跳过（不 warn、不触发 on_change）——
/// session/load 复用进程内 registry 重放时不刷「命令注册冲突」warn（P1-1）；
/// 真实冲突（不同条目抢占同键，如 `ui:clear` 与 `core:clear` 第一等级裸名
/// 互斥）仍由注册表纯拒绝裁决。
pub(crate) fn register_ui_entries(caps: &PeriCaps, command_registry: &CommandRegistry) {
    let existing: std::collections::HashSet<String> = command_registry
        .snapshot()
        .iter()
        .map(|e| e.fullname.to_lowercase())
        .collect();
    for entry in ui_route_entries(caps) {
        if existing.contains(&entry.fullname.to_lowercase()) {
            continue;
        }
        if let Err(e) = command_registry.register(entry) {
            tracing::warn!(error = ?e, "ui 条目注册冲突（拒绝，不覆盖）");
        }
    }
}

/// 注册表投影 → `AvailableCommandsUpdate`（单一事实源：RouteEntry；设计
/// 不变式 1——协议层不得再造一份命令表）。每条条目降维进 `_meta`（Phase 3
/// 方案确认字段映射表）：
///
/// | 设计字段 | `_meta` 键名 | 省略规则 |
/// |---|---|---|
/// | fullname（唯一键） | —（`name` 字段直接承载） | 恒有 |
/// | kind | `periKind`（`CommandEntryKind` serde snake_case） | 恒有 |
/// | level | `periLevel`（1 \| 2） | 恒有（显式，不留缺省歧义） |
/// | aliases | `periAliases` | 空数组省略 |
/// | category | `periCategory` | `None` 省略 |
/// | args | `periArgs`（`ArgsSchema` serde 形态） | `None` 省略 |
///
/// update 级 meta：`skillNames`（仅 core 域 Skill 条目 fullname，`caps.skill_names`
/// 门控保留）；`mcpSkillNames` 已退役（Phase 6 D1，03-protocol §4 建议——
/// kind 已入投影条目 `_meta.periKind`，Hub 按条目级 kind 消费），不再写入。
/// `caps.ui_commands` 不再在此附加任何条目（门控反转，ui 条目由调用点
/// 注册进注册表）。
///
/// 不做按名去重（Phase 6 D1）：词法统一后本地 = `core:{name}`、MCP =
/// `mcp:{server}:{skill}`、插件 = `plugin:{plugin}:{cmd}`，键空间两两不相交，
/// 键唯一性由注册表 register 时保证（A2 冲突纯拒绝），投影 = snapshot 全量。
pub(crate) fn build_available_commands_update(
    entries: &[Arc<RouteEntry>],
    caps: &PeriCaps,
) -> AvailableCommandsUpdate {
    let mut commands: Vec<AvailableCommand> = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut m = serde_json::Map::new();
        m.insert(
            "periKind".into(),
            serde_json::to_value(entry.kind).expect("CommandEntryKind 序列化不应失败"),
        );
        m.insert(
            "periLevel".into(),
            serde_json::json!(match entry.level() {
                CommandLevel::Level1 => 1,
                CommandLevel::Level2 => 2,
            }),
        );
        if !entry.aliases.is_empty() {
            m.insert(
                "periAliases".into(),
                serde_json::to_value(&entry.aliases).expect("aliases 序列化不应失败"),
            );
        }
        if let Some(category) = &entry.category {
            m.insert("periCategory".into(), serde_json::json!(category));
        }
        if let Some(args) = &entry.args_schema {
            m.insert(
                "periArgs".into(),
                serde_json::to_value(args).expect("ArgsSchema 序列化不应失败"),
            );
        }
        commands.push(
            AvailableCommand::new(entry.fullname.clone(), entry.description.clone()).meta(Some(m)),
        );
    }

    let mut meta = serde_json::Map::new();
    if caps.skill_names {
        let skill_names: Vec<serde_json::Value> = entries
            .iter()
            .filter(|e| e.kind == CommandEntryKind::Skill && e.provenance.source.domain() == "core")
            .map(|e| serde_json::Value::String(e.fullname.clone()))
            .collect();
        meta.insert("skillNames".into(), serde_json::Value::Array(skill_names));
    }

    let update = AvailableCommandsUpdate::new(commands);
    if meta.is_empty() {
        update
    } else {
        update.meta(meta)
    }
}
