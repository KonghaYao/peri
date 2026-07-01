// [TRAP] CommandRegistry::dispatch 借用限制
// &self + &mut App 冲突，当前用 std::mem::take + put-back 解决。修改 dispatch 时保留此模式。
pub mod core;
pub mod panel;
pub mod session;

pub use panel::agents;

/// 注册所有内置命令，返回配置好的 CommandRegistry
pub fn default_registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(Box::new(core::config::ConfigCommand));
    r.register(Box::new(core::clear::ClearCommand));
    r.register(Box::new(core::help::HelpCommand));
    r.register(Box::new(core::history::HistoryCommand));
    r.register(Box::new(core::exit::ExitCommand));
    r.register(Box::new(core::gc::GcCommand));

    r.register(Box::new(panel::model::ModelCommand));
    r.register(Box::new(panel::plugin::PluginCommand));
    r.register(Box::new(panel::mcp::McpCommand));
    r.register(Box::new(panel::hooks::HooksCommand));
    r.register(Box::new(panel::cron::CronCommand));
    r.register(Box::new(panel::agents::AgentsCommand));
    r.register(Box::new(panel::memory::MemoryCommand));
    r.register(Box::new(panel::login::LoginCommand));
    r.register(Box::new(panel::tasks::TasksCommand));
    r.register(Box::new(panel::betas::BetasCommand));
    r.register(Box::new(panel::workflows::WorkflowsCommand));
    r.register(Box::new(session::rename::RenameCommand));
    r.register(Box::new(session::channel::ChannelCommand));
    r.register(Box::new(session::context_cmd::ContextCommand));
    r.register(Box::new(session::cost::CostCommand));
    r.register(Box::new(session::lang::LangCommand));
    r.register(Box::new(session::effort::EffortCommand));
    r.register(Box::new(session::loop_cmd::LoopCommand));
    r.register(Box::new(session::setup::SetupCommand));
    r.register(Box::new(session::bg::BgCommand));
    r
}

use crate::app::App;
use crate::runtime::effect::Effect;

// ─── Command trait ────────────────────────────────────────────────────────────

pub trait Command: Send + Sync {
    /// 命令名，不含 /（如 "model"、"help"、"clear"）
    fn name(&self) -> &str;
    /// 单行描述，用于 /help 展示
    fn description(&self, lc: &crate::i18n::LcRegistry) -> String;
    /// 命令别名列表（不含 /），默认为空
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    /// 执行命令，args 是命令名之后的参数字符串（已 trim）。
    /// 返回副作用列表，由 main_loop 统一执行。
    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect>;
}

// ─── CommandRegistry ──────────────────────────────────────────────────────────

#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Box<dyn Command>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, cmd: Box<dyn Command>) {
        self.commands.push(cmd);
    }

    /// 注册插件提供的命令（从 PluginLoadResult 获取）
    pub fn register_plugin_commands(
        &mut self,
        commands: Vec<peri_middlewares::plugin::CommandEntry>,
    ) {
        for entry in commands {
            self.register(Box::new(
                session::plugin_command::PluginCommandAdapter::new(entry),
            ));
        }
    }

    /// 解析并执行命令。
    /// 输入格式："/name args..."
    /// 匹配优先级：精确匹配 > 别名精确匹配 > 前缀唯一匹配（支持 /m → /model）
    /// 返回 Some(effects) 表示找到命令并执行，None 表示未知命令或有歧义。
    pub fn dispatch(&self, app: &mut App, input: &str) -> Option<Vec<Effect>> {
        let input = input.trim_start_matches('/');
        let (name, args) = match input.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (input.trim(), ""),
        };

        // 1. 精确匹配
        if let Some(cmd) = self.commands.iter().find(|c| c.name() == name) {
            return Some(cmd.execute(app, args));
        }

        // 2. 别名精确匹配
        if let Some(cmd) = self.commands.iter().find(|c| c.aliases().contains(&name)) {
            return Some(cmd.execute(app, args));
        }

        // 3. 前缀唯一匹配（同时对 name 和 aliases）
        let matches: Vec<_> = self
            .commands
            .iter()
            .filter(|c| {
                c.name().starts_with(name) || c.aliases().iter().any(|a| a.starts_with(name))
            })
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].execute(app, args));
        }

        None
    }

    /// 返回所有已注册命令的 (name, description, aliases) 列表
    pub fn list(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String, Vec<String>)> {
        self.commands
            .iter()
            .map(|c| {
                (
                    c.name().to_string(),
                    c.description(lc),
                    c.aliases().into_iter().map(String::from).collect(),
                )
            })
            .collect()
    }

    /// 按前缀匹配命令，返回匹配的 (name, description) 列表
    /// prefix 不含 /，如 "mo" 匹配 "model"
    /// 同时匹配 name 和 aliases
    pub fn match_prefix(
        &self,
        prefix: &str,
        lc: &crate::i18n::LcRegistry,
    ) -> Vec<(String, String)> {
        self.commands
            .iter()
            .filter(|c| {
                c.name().starts_with(prefix) || c.aliases().iter().any(|a| a.starts_with(prefix))
            })
            .map(|c| (c.name().to_string(), c.description(lc)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::app::App;
    include!("mod_test.rs");
}
