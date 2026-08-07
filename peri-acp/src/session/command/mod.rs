//! ACP Slash Commands — 命令基础设施。
//!
//! 定义命令注册表、内置命令注册与执行入口。
//! 命令契约（[`AgentCommand`] / [`CommandContext`] / [`CommandResult`] /
//! [`BgForkRequest`] / [`BgForkSpawner`] / [`PromptStopReason`]）已随 L5
//! 迁入契约层（peri-acp-types::command），命令实现（bg / compact 执行体）
//! 迁入 Agent 层（peri-agent::session::exec）；本模块保留注册表与
//! 内置命令（clear / rewind / compact shim）。
//!
//! 命令在 executor 入口拦截，`Immediate` 类型不构建 agent 直接执行。

use std::sync::Arc;

pub mod bg;
pub mod clear;
pub mod compact;
pub mod rewind;

/// Rewind 文件复原相关符号——供 dispatch 层（`session/rewind-preview` 预算）
/// 复用 `extract_file_changes` / `FileChange`。
pub(crate) use rewind::{extract_file_changes, FileChange, RewindCommand};

/// 命令契约（L5：事实源 peri-acp-types::command）。
pub use peri_acp_types::command::{
    AgentCommand, BgForkRequest, BgForkSpawner, CommandContext, CommandKind, CommandResult,
    PromptStopReason,
};

/// 命令注册表。
pub struct CommandRegistry {
    // L5：Arc 存储——`find_arc` 为命令拦截注入面（peri-agent 侧
    // `command_lookup` 闭包）提供 owned 句柄，避免生命周期借用。
    commands: Vec<Arc<dyn AgentCommand>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn register(&mut self, cmd: Box<dyn AgentCommand>) {
        self.commands.push(Arc::from(cmd));
    }

    /// 按名称或别名查找命令。返回 `(命令引用, 剩余参数)`。
    pub fn find<'a>(&'a self, text: &'a str) -> Option<(&'a dyn AgentCommand, &'a str)> {
        let text = text.trim_start_matches('/');
        let (name, args) = match text.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (text.trim(), ""),
        };
        if name.is_empty() {
            return None;
        }

        // 1) 精确匹配 name
        for cmd in &self.commands {
            if cmd.name() == name {
                return Some((cmd.as_ref(), args));
            }
        }
        // 2) 前缀匹配 name（/rew → /rewind）。仅当唯一前缀时生效，多个歧义前缀退化为无匹配。
        let prefix_matches: Vec<&Arc<dyn AgentCommand>> = self
            .commands
            .iter()
            .filter(|cmd| cmd.name().starts_with(name) && cmd.name() != name)
            .collect();
        if prefix_matches.len() == 1 {
            return Some((prefix_matches[0].as_ref(), args));
        }
        // 3) 精确匹配 alias
        for cmd in &self.commands {
            if cmd.aliases().contains(&name) {
                return Some((cmd.as_ref(), args));
            }
        }
        None
    }

    /// 按名称或别名查找命令，返回 owned `Arc` 句柄（L5：命令拦截注入面——
    /// 经 `command_lookup` 闭包把注册表查找提升为 `'static`，匹配语义与
    /// [`find`] 完全一致）。
    pub fn find_arc(&self, text: &str) -> Option<(Arc<dyn AgentCommand>, String)> {
        let text = text.trim_start_matches('/');
        let (name, args) = match text.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (text.trim(), ""),
        };
        if name.is_empty() {
            return None;
        }

        // 1) 精确匹配 name
        if let Some(cmd) = self.commands.iter().find(|cmd| cmd.name() == name) {
            return Some((Arc::clone(cmd), args.to_string()));
        }
        // 2) 前缀匹配 name（/rew → /rewind）。仅当唯一前缀时生效，多个歧义前缀退化为无匹配。
        let prefix_matches: Vec<&Arc<dyn AgentCommand>> = self
            .commands
            .iter()
            .filter(|cmd| cmd.name().starts_with(name) && cmd.name() != name)
            .collect();
        if prefix_matches.len() == 1 {
            return Some((Arc::clone(prefix_matches[0]), args.to_string()));
        }
        // 3) 精确匹配 alias
        if let Some(cmd) = self
            .commands
            .iter()
            .find(|cmd| cmd.aliases().contains(&name))
        {
            return Some((Arc::clone(cmd), args.to_string()));
        }
        None
    }

    /// 返回所有注册命令的 `(name, description, aliases)` 元组。
    pub fn list(&self) -> Vec<(&str, &str, Vec<&str>)> {
        self.commands
            .iter()
            .map(|c| (c.name(), c.description(), c.aliases()))
            .collect()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        default_command_registry()
    }
}

/// 创建包含所有内置命令的默认注册表。
pub fn default_command_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(bg::BgCommand));
    reg.register(Box::new(compact::CompactCommand));
    reg.register(Box::new(clear::ClearCommand));
    reg.register(Box::new(rewind::RewindCommand));
    reg
}

/// 创建仅包含 agent 内部命令的注册表（供 prompt 拦截用）。
/// 视图层命令（/clear、/rewind）不在此注册表中——它们由 TUI kit 路径拦截处理。
pub fn default_prompt_command_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(compact::CompactCommand));
    reg.register(Box::new(bg::BgCommand));
    reg
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
