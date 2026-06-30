use crate::runtime::effect::Effect;
use peri_middlewares::plugin::CommandEntry;

use crate::{app::App, command::Command};

/// 将插件的 CommandEntry 适配为 TUI Command trait
pub struct PluginCommandAdapter {
    entry: CommandEntry,
}

impl PluginCommandAdapter {
    pub fn new(entry: CommandEntry) -> Self {
        Self { entry }
    }
}

impl Command for PluginCommandAdapter {
    fn name(&self) -> &str {
        &self.entry.name
    }
    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        self.entry.description.clone()
    }
    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        // 插件命令的行为是触发对应的 skill 预加载。
        // 保持用户原始输入（含命名空间前缀），让 SkillPreloadMiddleware 识别。
        let message = if args.is_empty() {
            format!("/{}", self.entry.name)
        } else {
            format!("/{} {}", self.entry.name, args)
        };

        // Cron #26 step 7e.7: UserBubble 路由到 v2 state.view。
        app.session_mgr
            .current_mut()
            .messages
            .push_user_bubble(message.clone());
        app.submit_message(message);
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use peri_middlewares::plugin::CommandSource;

    use super::*;
    use crate::i18n::LcRegistry;

    fn make_lc() -> LcRegistry {
        LcRegistry::new(None)
    }

    #[test]
    fn test_adapter_name_returns_entry_name() {
        let entry = CommandEntry {
            name: "test:cmd".into(),
            description: "desc".into(),
            source: CommandSource::Builtin,
        };
        let adapter = PluginCommandAdapter::new(entry);
        assert_eq!(adapter.name(), "test:cmd");
    }

    #[test]
    fn test_adapter_description_returns_entry_description() {
        let entry = CommandEntry {
            name: "test:cmd".into(),
            description: "my description".into(),
            source: CommandSource::Builtin,
        };
        let adapter = PluginCommandAdapter::new(entry);
        let lc = make_lc();
        assert_eq!(adapter.description(&lc), "my description");
    }
}
