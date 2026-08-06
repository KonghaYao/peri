//! Build ACP available commands list, shared by TUI and stdio transports.

use agent_client_protocol_schema::v1::AvailableCommand;
use peri_acp_types::skills::SkillMetadata;

/// Build the list of available slash commands for ACP clients,
/// including discovered skills as command entries using their plain name.
pub fn build_available_commands(skills: &[SkillMetadata]) -> Vec<AvailableCommand> {
    let mut commands = vec![
        AvailableCommand::new("help", "Show available commands and their descriptions"),
        AvailableCommand::new("clear", "Clear the current conversation"),
        AvailableCommand::new(
            "compact",
            "Compress the conversation history to save context",
        ),
        AvailableCommand::new("context", "Display context usage / token statistics"),
        AvailableCommand::new("cost", "Show token usage and estimated cost"),
        AvailableCommand::new("mode", "Switch the current permission mode"),
        AvailableCommand::new("effort", "Configure LLM reasoning/thinking effort"),
        AvailableCommand::new("loop", "Control agent iteration loop"),
        AvailableCommand::new("history", "View and resume previous conversations"),
        AvailableCommand::new("agents", "Manage sub-agent definitions"),
        AvailableCommand::new("rename", "Rename the current session"),
        AvailableCommand::new("lang", "Switch display language / locale"),
        AvailableCommand::new("exit", "Exit the application"),
    ];
    for skill in skills {
        commands.push(AvailableCommand::new(
            skill.name.clone(),
            skill.description.clone(),
        ));
    }
    commands
}
