//! Build ACP available commands list, shared by TUI and stdio transports.

use agent_client_protocol_schema::v1::{AvailableCommand, AvailableCommandsUpdate};
use peri_acp_types::skills::SkillMetadata;
use peri_acp_types::PeriCaps;

/// 界面性命令（TUI 本地处理，`peri-tui` `submit_request.rs` 拦截；无 host
/// 执行语义）。仅当客户端声明 `peri.uiCommands` cap（TUI）时附加广播——
/// 外部客户端（IDE 等）收到这些条目无意义，不声明则不传递。
const UI_COMMANDS: &[(&str, &str)] = &[
    ("help", "Show available commands and their descriptions"),
    ("clear", "Clear the current conversation"),
    ("context", "Display context usage / token statistics"),
    ("cost", "Show token usage and estimated cost"),
    ("mode", "Switch the current permission mode"),
    ("effort", "Configure LLM reasoning/thinking effort"),
    ("history", "View and resume previous conversations"),
    ("agents", "Manage sub-agent definitions"),
    ("rename", "Rename the current session"),
    ("lang", "Switch display language / locale"),
    ("exit", "Exit the application"),
];

/// Build the list of available slash commands for ACP clients,
/// including discovered skills as command entries using their plain name.
///
/// 基座为**功能性**命令（compact / loop）+ skills，对所有客户端广播；
/// 界面性命令由 [`build_available_commands_update`] 按 `caps.ui_commands`
/// 门控附加（仅 TUI 场景）。
pub fn build_available_commands(skills: &[SkillMetadata]) -> Vec<AvailableCommand> {
    let mut commands = vec![
        AvailableCommand::new(
            "compact",
            "Compress the conversation history to save context",
        ),
        AvailableCommand::new("loop", "Control agent iteration loop"),
    ];
    for skill in skills {
        commands.push(AvailableCommand::new(
            skill.name.clone(),
            skill.description.clone(),
        ));
    }
    commands
}

/// 本地 + MCP 合并构建 `AvailableCommandsUpdate`（stdio/notify 两路径共用，
/// DD-5）。availableCommands = 2 内置功能性（compact / loop）+ 每 skill 一条
/// （MCP 条目 name/description 同构）；`caps.ui_commands`（TUI 全 cap 声明）
/// 时附加界面性命令（help / clear / mode / lang / exit 等 11 条）——外部
/// 客户端不声明则不传递；meta = `skillNames`（仅本地名，MCP 名不进）+
/// `mcpSkillNames`（mcp 非空时附加，不加 cap 门控——TUI 全 cap，外部客户端
/// 忽略未知 meta key）。键序不保证（客户端按键解析，顺序非契约）。
pub(crate) fn build_available_commands_update(
    local: &[SkillMetadata],
    mcp: &[SkillMetadata],
    caps: &PeriCaps,
) -> AvailableCommandsUpdate {
    let mut seen: std::collections::HashSet<String> =
        local.iter().map(|s| s.name.to_lowercase()).collect();
    let mut merged: Vec<SkillMetadata> = local.to_vec();
    for mcp_skill in mcp {
        // 本地优先、按名去重（与 Skills 缓存合并语义一致，skills/mod.rs）：
        // 本地 skill 恰名 `mcp__x__y` 时保留本地条目，避免 availableCommands 同名双显示。
        if seen.insert(mcp_skill.name.to_lowercase()) {
            merged.push(mcp_skill.clone());
        }
    }
    let mut commands = build_available_commands(&merged);
    if caps.ui_commands {
        commands.extend(
            UI_COMMANDS
                .iter()
                .map(|(name, desc)| AvailableCommand::new(*name, *desc)),
        );
    }

    let mut meta = serde_json::Map::new();
    if caps.skill_names {
        meta.insert(
            "skillNames".to_string(),
            serde_json::Value::Array(
                local
                    .iter()
                    .map(|s| serde_json::Value::String(s.name.clone()))
                    .collect(),
            ),
        );
    }
    if !mcp.is_empty() {
        meta.insert(
            "mcpSkillNames".to_string(),
            serde_json::Value::Array(
                mcp.iter()
                    .map(|s| serde_json::Value::String(s.name.clone()))
                    .collect(),
            ),
        );
    }

    let update = AvailableCommandsUpdate::new(commands);
    if meta.is_empty() {
        update
    } else {
        update.meta(meta)
    }
}
