use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

use crate::skills::{loader::resolve_skill_roots, scan_skill_roots, SkillRoot};

/// 从文本中提取 `/skill-name` 模式的 skill 名称
///
/// 支持格式：
/// - `/skill-name` — 单个 skill
/// - `/skill-a /skill-b` — 多个 skill（空格分隔）
/// - `/namespace:skill-name` — 带命名空间的 skill
/// - 消息中任意位置出现即可（不限于行首）
///
/// 匹配由 `/` 开头、后跟 `[a-zA-Z0-9_:.-]` 的 token。
/// 允许 `:` 以支持插件命名空间（如 `/ecc:plan`）。
pub fn extract_skill_names_from_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|word| {
            let name = word.strip_prefix('/')?;
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
            {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// SkillPreloadMiddleware - 将指定 skill 全文以 fake Read 工具调用注入到 agent state
///
/// 在 `before_agent` 时，根据 `skill_names` 列表找到对应 SKILL.md 文件，
/// 将其内容以 Ai[ToolUse] → Tool[ToolResult] 消息序列追加到用户消息之后（executor
/// 在 `before_agent` 之前已将用户消息 `add_message` 到 state），使 LLM 从第一轮推理
/// 就能看到完整 skill 内容。
///
/// 使用 `add_message` 而非 `prepend_message`，确保工具调用出现在用户消息之后，
/// 不影响 Anthropic messages 数组的 prompt cache（cache_control 在第一条 user 消息上）。
///
/// # 注入消息结构
///
/// ```text
/// [Human "用户消息"]  ← 已由 executor 添加
/// [Ai]    [ToolUse{Read, call_{hex}}, ToolUse{Read, call_{hex}}, ...]
/// [Tool]  ToolResult{call_{hex}, skill_0_content}
/// [Tool]  ToolResult{call_{hex}, skill_1_content}
/// ...
/// ```
///
/// 找不到的 skill 名称静默跳过，不报错。
pub struct SkillPreloadMiddleware {
    skill_names: Vec<String>,
    cwd: String,
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
}

impl SkillPreloadMiddleware {
    pub fn new(skill_names: Vec<String>, cwd: &str) -> Self {
        Self {
            skill_names,
            cwd: cwd.to_string(),
            plugin_roots: Vec::new(),
            disable_bundled: false,
        }
    }

    /// 追加插件 skills 搜索根（每个 root 携带 source 与 plugin_name）
    pub fn with_plugin_roots(mut self, roots: Vec<SkillRoot>) -> Self {
        self.plugin_roots = roots;
        self
    }

    /// 设置是否禁用 builtin skill（默认 false）
    pub fn with_disable_bundled(mut self, disable: bool) -> Self {
        self.disable_bundled = disable;
        self
    }
}

#[async_trait]
impl Middleware for SkillPreloadMiddleware {
    fn name(&self) -> &str {
        "SkillPreloadMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 确定要预加载的 skill 名称列表
        let skill_names = if !self.skill_names.is_empty() {
            // SubAgent 路径：使用构造时传入的显式列表
            self.skill_names.clone()
        } else {
            // 主 Agent 路径：从最后一条 Human 消息中自动检测 /skill-name token
            let last_human = state
                .messages()
                .iter()
                .rev()
                .find(|m| matches!(m, BaseMessage::Human { .. }));
            match last_human {
                Some(msg) => extract_skill_names_from_text(&msg.content()),
                None => return Ok(()),
            }
        };

        if skill_names.is_empty() {
            return Ok(());
        }

        let roots = resolve_skill_roots(&self.cwd, self.plugin_roots.clone(), self.disable_bundled);
        let names_lower: Vec<String> = skill_names.iter().map(|s| s.to_lowercase()).collect();

        // 在 blocking 线程中扫描目录 + 读取文件内容
        let skill_contents = tokio::task::spawn_blocking(move || {
            let all_skills = scan_skill_roots(&roots);
            all_skills
                .into_iter()
                .filter(|s| {
                    let skill_name_lower = s.name.to_lowercase();
                    names_lower.iter().any(|name| {
                        // 精确匹配（/plan）
                        skill_name_lower == *name
                        // 或去掉命名空间前缀后匹配（/ecc:plan → plan）
                        || name.rsplit_once(':').map(|(_, n)| n.to_lowercase()).as_deref() == Some(&skill_name_lower)
                    })
                })
                .filter_map(|s| {
                    // Builtin source 走常量数组查找（虚拟路径无文件），其他 source 走磁盘读取
                    // 注意：本文件在 peri-middlewares crate 内部，必须用 crate:: 路径
                    // （不能用 peri_middlewares::，否则编译失败）
                    let content = if matches!(s.source, crate::skills::SkillSource::Builtin) {
                        crate::skills::builtin::BUILTIN_SKILLS
                            .iter()
                            .find(|bs| bs.name == s.name)
                            .map(|bs| bs.content.to_string())
                    } else {
                        std::fs::read_to_string(&s.path).ok()
                    };
                    content.map(|c| (s.path.to_string_lossy().to_string(), c))
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
            middleware: "SkillPreloadMiddleware".to_string(),
            reason: format!("spawn_blocking 失败: {e}"),
        })?;

        if skill_contents.is_empty() {
            return Ok(());
        }

        // Generate tool_call_ids: call_{uuid hex without hyphens, 32 chars}
        let call_ids: Vec<String> = (0..skill_contents.len())
            .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
            .collect();

        // 构造 Ai 消息的 ToolUse ContentBlock 列表
        let tool_use_blocks: Vec<ContentBlock> = skill_contents
            .iter()
            .zip(call_ids.iter())
            .map(|((path, _), id)| {
                ContentBlock::tool_use(id.clone(), "Read", serde_json::json!({ "path": path }))
            })
            .collect();

        // 追加 Ai 消息（ai_from_blocks 自动双写 tool_calls）
        state.add_message(BaseMessage::ai_from_blocks(tool_use_blocks));

        // 追加 Tool 结果消息
        for (id, (_, content)) in call_ids.iter().zip(skill_contents.iter()) {
            state.add_message(BaseMessage::tool_result(id.clone(), content.clone()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use peri_agent::{agent::state::AgentState, middleware::r#trait::Middleware};
    use tempfile::tempdir;

    use super::*;
    include!("skill_preload_test.rs");
}
