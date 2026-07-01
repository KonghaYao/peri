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
                vec![Effect::ShowNotification(format!(
                    "Marketplace add: {} (v2 plugin panel pending)",
                    input
                ))]
            }

            // /plugin install <name@marketplace>
            ["install", name_at_marketplace] => {
                let (name, marketplace) = name_at_marketplace
                    .split_once('@')
                    .unwrap_or((name_at_marketplace, "claude-plugins-official"));
                vec![Effect::ShowNotification(format!(
                    "Plugin install: {}@{} (v2 plugin panel pending)",
                    name, marketplace
                ))]
            }

            // /plugin marketplace update <name>
            ["marketplace", "update", name] => {
                vec![Effect::ShowNotification(format!(
                    "Marketplace update: {} (v2 plugin panel pending)",
                    name
                ))]
            }

            // 未知用法 → 显示帮助
            _ => {
                let help = app.services.lc.tr("command-plugin-help");
                vec![Effect::ShowNotification(help.to_string())]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!("plugin_test.rs");
}
