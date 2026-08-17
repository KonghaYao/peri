use async_trait::async_trait;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

use crate::skills::{SkillMetadata, SkillRoot, SkillSource};

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

/// SkillPreloadMiddleware - 将指定 skill 全文以 fake SkillTool 调用注入到 agent state
///
/// 在 `before_agent` 时，根据 `skill_names` 列表找到对应 SKILL.md 文件，
/// 将其内容以 Ai[ToolUse{SkillTool}] → Tool[ToolResult] 消息序列追加到用户消息之后（executor
/// 在 `before_agent` 之前已将用户消息 `add_message` 到 state），使 LLM 从第一轮推理
/// 就能看到完整 skill 内容。
///
/// 注入的 ToolUse 名为 `SkillTool`（与会话中真实注册的统一 skill 加载协议一致，
/// 见 D3：`Skill(skill, args)` 已移除，模型可见协议只剩 `SkillTool(skill_name)` +
/// `DiscoverSkillsTool`），input 为 `{"skill_name": <名称>}`。
///
/// 使用 `add_message` 而非 `prepend_message`，确保工具调用出现在用户消息之后，
/// 不影响 Anthropic messages 数组的 prompt cache（cache_control 在第一条 user 消息上）。
///
/// # 注入消息结构
///
/// ```text
/// [Human "用户消息"]  ← 已由 executor 添加
/// [Ai]    [ToolUse{SkillTool, call_{hex}}, ToolUse{SkillTool, call_{hex}}, ...]
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
    /// 会话级 MCP skill 远端注册表（None = 仅本地磁盘路径；默认 None，
    /// `new()` 签名与既有测试/构造点不变）。
    mcp_registry: Option<std::sync::Arc<McpSkillRegistry>>,
}

impl SkillPreloadMiddleware {
    pub fn new(skill_names: Vec<String>, cwd: &str) -> Self {
        Self {
            skill_names,
            cwd: cwd.to_string(),
            plugin_roots: Vec::new(),
            disable_bundled: false,
            mcp_registry: None,
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

    /// 注入 MCP 远端技能注册表（None = 仅本地磁盘路径；默认 None）。
    pub fn with_mcp_registry(mut self, reg: Option<std::sync::Arc<McpSkillRegistry>>) -> Self {
        self.mcp_registry = reg;
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

        let cwd = self.cwd.clone();
        let plugin_roots = self.plugin_roots.clone();
        let disable_bundled = self.disable_bundled;
        let names_lower: Vec<String> = skill_names.iter().map(|s| s.to_lowercase()).collect();

        // DD-3：每名称先查 registry（同步 RwLock 读，无需 spawn_blocking）。
        // `registry.find` 已实现精确名（mcp__<server>__<skill>）+ `<server>:<skill>`
        // 别名；命中即用缓存 content，未命中才走既有本地磁盘路径。
        // 兜底（决策 1 + A3）：`{server}:{skill}` 命令形态 token 再经
        // `registry.find_by_command` 按「server 名末段小写」匹配——覆盖
        // plugin 多冒号 server key（`plugin:{plugin}:{server}`）下 `find`
        // 别名拼原名必 miss 的场景（命令面 fullname 同源取末段）。
        // OQ1：`mcp__` 前缀 token 是 MCP 身份——registry miss 即静默跳过，
        // 不回退磁盘（防误注入本地同名内容）；`<x>:<y>` 别名 miss 保持既有
        // 磁盘回退（对齐 plugin 命名空间语义）。
        let mut registry_hits: std::collections::HashMap<String, SkillMetadata> =
            std::collections::HashMap::new();
        let mut skipped_mcp_miss: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut disk_names: Vec<String> = Vec::new();
        if let Some(reg) = &self.mcp_registry {
            for name in &names_lower {
                match reg.find(name).or_else(|| reg.find_by_command(name)) {
                    Some(meta) => {
                        registry_hits.insert(name.clone(), meta);
                    }
                    None if name.starts_with("mcp__") => {
                        // OQ1：mcp__ 前缀 registry miss → 跳过，不回退磁盘。
                        skipped_mcp_miss.insert(name.clone());
                    }
                    None => disk_names.push(name.clone()),
                }
            }
        } else {
            disk_names = names_lower.clone();
        }

        // 在 blocking 线程中查找并读取本地 skill 内容
        // 委托给 skills::loader::find_skill_content 公共函数，避免重复实现。
        // 位置保留（map 而非 filter_map）：Option 与 disk_names 逐位对齐，
        // 合并循环按名消费，保证输出与用户原始输入顺序一致（miss 夹在命中
        // 之间也不移位）。
        let disk_resolved: Vec<Option<(SkillMetadata, String)>> =
            tokio::task::spawn_blocking(move || {
                disk_names
                    .iter()
                    .map(|name| {
                        // 精确匹配优先，再用命名空间后缀匹配（/ecc:plan → plan）
                        crate::skills::loader::find_skill_content(
                            &cwd,
                            plugin_roots.clone(),
                            disable_bundled,
                            name,
                        )
                        .or_else(|| {
                            name.rsplit_once(':').and_then(|(_, suffix)| {
                                crate::skills::loader::find_skill_content(
                                    &cwd,
                                    plugin_roots.clone(),
                                    disable_bundled,
                                    suffix,
                                )
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "SkillPreloadMiddleware".to_string(),
                reason: format!("spawn_blocking 失败: {e}"),
            })?;

        // 按原始输入顺序合并两路结果（registry 命中与磁盘命中交错保持用户
        // 输入顺序）；registry 命中但 content 缺失（理论不可能）静默跳过。
        // disk_iter 与 disk_names 逐位对齐（位置保留），仅在"非 registry 命中、
        // 非 mcp__ skip"的名称上消费，避免跳过项消耗后续名称的磁盘条目。
        let mut disk_iter = disk_resolved.into_iter();
        let mut skill_contents: Vec<(String, SkillMetadata, String)> =
            Vec::with_capacity(names_lower.len());
        for name in &names_lower {
            if let Some(meta) = registry_hits.get(name) {
                if let Some(content) = meta.content.as_deref() {
                    skill_contents.push((name.clone(), meta.clone(), content.to_string()));
                }
            } else if skipped_mcp_miss.contains(name) {
                // OQ1：mcp__ 前缀 miss 不回退磁盘——静默跳过（不消费 disk_iter）。
            } else if let Some(Some((meta, content))) = disk_iter.next() {
                skill_contents.push((name.clone(), meta, content));
            }
        }

        if skill_contents.is_empty() {
            return Ok(());
        }

        // Generate tool_call_ids: call_{uuid hex without hyphens, 32 chars}
        let call_ids: Vec<String> = (0..skill_contents.len())
            .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
            .collect();

        // 构造 Ai 消息的 ToolUse ContentBlock 列表（fake SkillTool 工具调用）
        let tool_use_blocks: Vec<ContentBlock> = skill_contents
            .iter()
            .zip(call_ids.iter())
            .map(|((name, _, _), id)| {
                ContentBlock::tool_use(
                    id.clone(),
                    "SkillTool",
                    serde_json::json!({ "skill_name": name }),
                )
            })
            .collect();

        // 追加 Ai 消息（ai_from_blocks 自动双写 tool_calls）
        state.add_message(BaseMessage::ai_from_blocks(tool_use_blocks));

        // 追加 Tool 结果消息；MCP 来源内容包裹来源标注（提示注入防御，验收 12）
        for (id, (_, meta, content)) in call_ids.iter().zip(skill_contents.iter()) {
            let content = if matches!(meta.source, SkillSource::Mcp) {
                crate::skills::annotate_mcp_content(meta, content)
            } else {
                content.clone()
            };
            state.add_message(BaseMessage::tool_result(id.clone(), content));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "skill_preload_test.rs"]
mod tests;
