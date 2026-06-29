//! 命名 Workflow 命令——扫描 `.claude/workflows/<name>.js` 注册 `/<name>` 命令。
//!
//! GAP-09：设计第 9 节"命名 Workflow"。
//!
//! 当用户输入 `/<name>` 时，命令读取 workflow 脚本文件内容，
//! 提交一个指令给 LLM，由 LLM 调用 Workflow 工具执行。

use std::path::PathBuf;

use crate::runtime::effect::Effect;
use crate::{app::App, command::Command, ui::message_view::MessageViewModel};

/// 扫描 `.claude/workflows/` 目录，返回 `(name, path)` 列表（按 name 排序）。
///
/// 支持 `.js`、`.mjs`、`.ts` 扩展名。
/// 目录不存在或为空时返回空 Vec（静默失败，不报错）。
pub fn discover_named_workflows(cwd: &str) -> Vec<(String, PathBuf)> {
    let workflows_dir = PathBuf::from(cwd).join(".claude").join("workflows");
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if matches!(ext, "js" | "mjs" | "ts") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            result.push((stem.to_string(), path));
                        }
                    }
                }
            }
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// 命名 Workflow 命令——持有 workflow 名称和文件路径。
///
/// 实现 `Command` trait，`name()` 返回 workflow 名称（不含 `/`），
/// 执行时读取脚本文件并提交指令给 LLM。
pub struct NamedWorkflowCommand {
    name: String,
    path: PathBuf,
}

impl NamedWorkflowCommand {
    pub fn new(name: String, path: PathBuf) -> Self {
        Self { name, path }
    }
}

impl Command for NamedWorkflowCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        format!("Run the '{}' workflow", self.name)
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let script = match std::fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(e) => {
                let vm = MessageViewModel::system(format!(
                    "Failed to read workflow file '{}': {}",
                    self.path.display(),
                    e
                ));
                app.session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .push(vm);
                app.render_rebuild();
                return vec![];
            }
        };

        // 构建指令提交给 LLM——LLM 调用 Workflow 工具执行该脚本。
        // 设计 9.2：命令执行时提交 "运行 workflow <name>" 给 LLM，LLM 调用 Workflow 工具。
        let prompt = if args.trim().is_empty() {
            format!(
                "Please run the workflow named '{}' by invoking the `Workflow` tool. \
                 Pass the following script content as the `script` parameter:\n\n\
                 ```javascript\n{}\n```",
                self.name, script
            )
        } else {
            format!(
                "Please run the workflow named '{}' with the following user arguments: {}\n\n\
                 Invoke the `Workflow` tool with the script content below as the `script` parameter, \
                 and parse the user arguments into a JSON object for the `args` parameter:\n\n\
                 ```javascript\n{}\n```",
                self.name, args, script
            )
        };

        app.submit_message(prompt);
        vec![]
    }
}

/// 注册 `.claude/workflows/` 下的所有命名 Workflow 命令到 registry。
///
/// 在 `App::new()` 和 `new_session()` 中调用，紧随 `default_registry()` 之后。
pub fn register_named_workflow_commands(cwd: &str, registry: &mut crate::command::CommandRegistry) {
    for (name, path) in discover_named_workflows(cwd) {
        registry.register(Box::new(NamedWorkflowCommand::new(name, path)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_named_workflows_empty_when_no_dir() {
        let tmp = std::env::temp_dir().join("peri_workflow_test_empty_12345");
        // 确保目录不存在
        let _ = std::fs::remove_dir_all(&tmp);
        let result = discover_named_workflows(tmp.to_str().unwrap());
        assert!(result.is_empty(), "non-existent dir should return empty");
    }

    #[test]
    fn test_discover_named_workflows_finds_js_files() {
        let tmp = std::env::temp_dir().join("peri_workflow_test_find_12345");
        let wf_dir = tmp.join(".claude").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("code-review.js"),
            "export const meta = { name: 'test' }",
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("deploy.mjs"),
            "export const meta = { name: 'deploy' }",
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("refactor.ts"),
            "export const meta = { name: 'refactor' }",
        )
        .unwrap();
        // 非 workflow 文件应被忽略
        std::fs::write(wf_dir.join("readme.md"), "# workflows").unwrap();
        std::fs::write(wf_dir.join("config.json"), "{}").unwrap();

        let result = discover_named_workflows(tmp.to_str().unwrap());

        // 清理
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "code-review");
        assert_eq!(result[1].0, "deploy");
        assert_eq!(result[2].0, "refactor");
    }

    #[test]
    fn test_discover_named_workflows_sorted() {
        let tmp = std::env::temp_dir().join("peri_workflow_test_sort_12345");
        let wf_dir = tmp.join(".claude").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        // 按乱序写入
        std::fs::write(wf_dir.join("zebra.js"), "// z").unwrap();
        std::fs::write(wf_dir.join("alpha.js"), "// a").unwrap();
        std::fs::write(wf_dir.join("mango.js"), "// m").unwrap();

        let result = discover_named_workflows(tmp.to_str().unwrap());

        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "alpha");
        assert_eq!(result[1].0, "mango");
        assert_eq!(result[2].0, "zebra");
    }
}
