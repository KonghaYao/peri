//! ui 域命令单源模块（设计文档 §88：ui 域归属 TUI，上送注册 + 本地拦截执行）。
//!
//! 这是 ui 域命令的**唯一事实源**（收敛 G5 四处名字表：PANELS / submit_request
//! match / 服务端 UI_COMMANDS / setup 合成）：
//! - [`ui_command_specs`]：本地字面量表（&'static）——上送注册与渲染共用；
//! - [`resolve_ui_command`]：裸名 / `ui:` 前缀 / aliases 归一化查找——提交与
//!   补全选中路径统一走这里，本地拦截执行（不发 ACP）。
//!
//! 别名表（`history` / `resume` / `his` → ThreadBrowser）自 panel_registry.rs
//! 迁入；panel_registry 的 `panel_for_slash_command` 反查函数已删除，命中路径
//! 统一收敛至本模块。

use crate::app::panel_types::PanelKind;
use crate::kit::panel_registry::PANELS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommandAction {
    OpenPanel(PanelKind),
    ToggleSetup,
}

/// 本地内部表（仅渲染 / 拦截用；与 Phase 3 契约类型 `peri_caps::UiCommandSpec`
/// 同名异构——本类型为 &'static 字面量表，**勿直接序列化**，上送时经转换层
/// 构造契约形态（String 字段 + args: None），P2-2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

/// ThreadBrowser 面板别名——自 panel_registry.rs:389-391 迁入（`/history`、
/// `/resume`、`/his` 都是 `/threads` 的别名）。
const THREAD_ALIASES: &[&str] = &["history", "resume", "his"];

/// ui 域命令清单 = PANELS（`slash_command` 非空）+ `/setup` + 面板别名。
///
/// 面板条目直接从 [`PANELS`] 表投影（数据同源：`slash_command` → 注册名、
/// `description` → 注册描述），别名挂在对应面板条目上。
pub fn ui_command_specs() -> Vec<UiCommandSpec> {
    let mut specs: Vec<UiCommandSpec> = PANELS
        .iter()
        .filter(|m| !m.slash_command.is_empty())
        .map(|m| UiCommandSpec {
            name: m.slash_command,
            aliases: if m.kind == PanelKind::ThreadBrowser {
                THREAD_ALIASES
            } else {
                &[]
            },
            description: m.description,
        })
        .collect();
    specs.push(UiCommandSpec {
        name: "setup",
        aliases: &[],
        description: "Open setup wizard to configure providers",
    });
    specs
}

/// 裸名 / `ui:` 前缀 / aliases 归一化查找。
///
/// - 裸名（`history` / `model`）：ui 域快捷形态，查 PANELS + 别名表 + setup；
/// - `ui:` 显式形态（`ui:history`）：剥除 `ui:` 前缀后仅允许 1 段（设计 §52
///   层数上限：2 段冒号），多层形态（`ui:foo:bar`）显式拒绝；
/// - **非 ui 域显式形态（`core:compact` / `mcp:demo:hello` / 未知域）一律
///   fall through（返回 None）**——TUI 只拦截 ui 域，其他域显式提交由 ACP
///   侧解析（设计 §78：词法非法同样 fall through，不报错）。
pub fn resolve_ui_command(name: &str) -> Option<UiCommandAction> {
    let normalized = name.trim().to_ascii_lowercase();
    let bare = match normalized.strip_prefix("ui:") {
        // ui 域层数上限 2 段（`ui:` + 1 段名）——rest 仍含冒号即词法非法，
        // 显式 fall through，不依赖 PANELS 空表兜底
        Some(rest) if !rest.contains(':') => rest,
        Some(_) => return None,
        None if !normalized.contains(':') => normalized.as_str(),
        None => return None,
    };
    if bare == "setup" {
        return Some(UiCommandAction::ToggleSetup);
    }
    if THREAD_ALIASES.contains(&bare) {
        return Some(UiCommandAction::OpenPanel(PanelKind::ThreadBrowser));
    }
    PANELS
        .iter()
        // slash_command 为空 = 无 slash 入口（AskUser / SubAgentDetail），
        // 空输入不得命中这类面板
        .find(|m| !m.slash_command.is_empty() && m.slash_command == bare)
        .map(|m| UiCommandAction::OpenPanel(m.kind))
}

#[cfg(test)]
#[path = "ui_command_test.rs"]
mod tests;
