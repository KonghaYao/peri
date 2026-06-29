use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct PluginCommand;

impl Command for PluginCommand {
    fn name(&self) -> &str {
        "plugin"
    }
    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-plugin-description")
    }
    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let parts: Vec<&str> = args.split_whitespace().collect();
        match parts.as_slice() {
            // /plugin（无参数）→ v2: PluginPanel managed by state machine
            [] => {
                vec![Effect::OpenPanel(PanelKind::Plugin), Effect::Render]
            }

            // /plugin marketplace add <url>
            ["marketplace", "add", rest @ ..] if !rest.is_empty() => {
                let input = rest.join(" ");
                // v2: marketplace operations deferred until PluginPanel v2 is wired
                app.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(format!(
                        "Marketplace add: {} (v2 plugin panel pending)",
                        input
                    ));
                vec![Effect::Render]
            }

            // /plugin install <name@marketplace>
            ["install", name_at_marketplace] => {
                let (name, marketplace) = name_at_marketplace
                    .split_once('@')
                    .unwrap_or((name_at_marketplace, "claude-plugins-official"));
                app.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(format!(
                        "Plugin install: {}@{} (v2 plugin panel pending)",
                        name, marketplace
                    ));
                vec![Effect::Render]
            }

            // /plugin marketplace update <name>
            ["marketplace", "update", name] => {
                app.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(format!(
                        "Marketplace update: {} (v2 plugin panel pending)",
                        name
                    ));
                vec![Effect::Render]
            }

            // 未知用法 → 显示帮助
            _ => {
                let help = app.services.lc.tr("command-plugin-help");
                app.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(help.to_string());
                vec![Effect::Render]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!("plugin_test.rs");
}
