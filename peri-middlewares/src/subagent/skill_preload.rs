use async_trait::async_trait;
use peri_acp_types::mcp_skills::{McpSkillRegistry, SkillLookup};
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::r#trait::Middleware,
};

use crate::skills::SkillRoot;

/// 从文本中提取 `/skill-name` 模式的 skill 名称
///
/// 支持格式：
/// - `/skill-name` — 单个 skill
/// - `/skill-a /skill-b` — 多个 skill（空格分隔）
/// - `/namespace:skill-name` — 带命名空间的 skill
/// - 消息中任意位置出现即可（不限于行首）
///
/// 匹配由 `/` 开头、后跟 `[a-zA-Z0-9_:.-]` 的 token。
/// `:` 保留给 MCP 的 `/server:skill` 命令形式；本地 SKILL.md 名称在扫描时已
/// 规范为连字符，因而不会以冒号形式命中本地 skill。
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

/// 预加载槽位：本地命中携带已读正文；MCP 命中携带条目（正文在闭包外经
/// 统一 activation 读取——资源读取是异步的，不能在 blocking 闭包里做）。
enum PreloadSlot {
    Ready {
        name: String,
        content: String,
    },
    Mcp {
        name: String,
        /// 盒装：SkillMetadata 远大于本地正文槽位（clippy large_enum_variant）。
        meta: Box<peri_acp_types::skills::SkillMetadata>,
    },
}

#[async_trait]
impl Middleware for SkillPreloadMiddleware {
    fn name(&self) -> &str {
        "SkillPreloadMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
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
        let registry = self.mcp_registry.clone();
        // 一批预加载共享一次本地扫描。按输入顺序产出槽位，避免把 registry
        // 命中、磁盘 miss 与磁盘命中拆成多路后再按位置合并。
        let (slots, ambiguous) = tokio::task::spawn_blocking(move || {
            let mut local_skills = None;
            let mut slots = Vec::with_capacity(skill_names.len());
            let mut ambiguous: Vec<(String, Vec<peri_acp_types::skills::SkillMetadata>)> =
                Vec::new();
            for name in skill_names {
                let name = name.to_lowercase();
                if let Some(reg) = &registry {
                    // registry-first：全名/别名（lookup_exact）→ 命令形态
                    // （lookup_by_command，保留 plugin 多冒号 server 的末段别名）。
                    // 命中后正文一律经统一 activation（W2），读取在闭包外异步进行；
                    // 激活失败不回落磁盘、不静默注入旧缓存。
                    //
                    // 歧义（同一形态命中多个 origin）→ 显式拒绝并列出候选：
                    // 不静默取首个，也不改走本地兜底（同名跨源必须消歧）。
                    let lookup = match reg.lookup_exact(&name) {
                        SkillLookup::Missing => reg.lookup_by_command(&name),
                        other => other,
                    };
                    match lookup {
                        SkillLookup::Found(meta) => {
                            slots.push(PreloadSlot::Mcp {
                                name,
                                meta: Box::new(*meta),
                            });
                            continue;
                        }
                        SkillLookup::Ambiguous(candidates) => {
                            ambiguous.push((name, candidates));
                            continue;
                        }
                        SkillLookup::Missing => {}
                    }
                    // mcp__ 表示 MCP 身份；registry miss 不回退磁盘。
                    if name.starts_with("mcp__") {
                        continue;
                    }
                }
                let skills = local_skills.get_or_insert_with(|| {
                    let roots = crate::skills::resolve_skill_roots(
                        &cwd,
                        plugin_roots.clone(),
                        disable_bundled,
                    );
                    crate::skills::scan_skill_roots(&roots)
                });
                let found = crate::skills::find_skill_in_list(skills, &name).or_else(|| {
                    name.rsplit_once(':')
                        .and_then(|(_, suffix)| crate::skills::find_skill_in_list(skills, suffix))
                });
                if let Some((_, content)) = found {
                    slots.push(PreloadSlot::Ready { name, content });
                }
            }
            (slots, ambiguous)
        })
        .await
        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
            middleware: "SkillPreloadMiddleware".to_string(),
            reason: format!("spawn_blocking 失败: {e}"),
        })?;

        // 歧义命中：显式拒绝（不注入、不回落本地），候选写入日志供消歧。
        for (name, candidates) in &ambiguous {
            tracing::warn!(
                skill = %name,
                candidates = %crate::mcp::skill_discovery::candidate_list(candidates),
                "MCP skill 预加载命中多个 origin，显式拒绝注入（请用完整名消歧）"
            );
        }

        // MCP 槽位：统一 activation（resources/read + digest/frontmatter 校验；
        // stale 经 skills/get 刷新一次）。失败静默跳过（与 miss 语义一致），
        // 不回退磁盘或发现期缓存。
        let mut skill_contents: Vec<(String, String)> = Vec::with_capacity(slots.len());
        for slot in slots {
            match slot {
                PreloadSlot::Ready { name, content } => skill_contents.push((name, content)),
                PreloadSlot::Mcp { name, meta } => {
                    let Some(reg) = self.mcp_registry.as_ref() else {
                        continue;
                    };
                    match crate::mcp::skill_activation::activate(reg, meta.as_ref(), None).await {
                        Ok(content) => skill_contents.push((
                            name,
                            crate::skills::annotate_mcp_content(meta.as_ref(), &content),
                        )),
                        Err(error) => tracing::debug!(
                            skill = %name,
                            reason = error.reason(),
                            "MCP skill 预加载激活失败，跳过注入"
                        ),
                    }
                }
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
            .map(|((name, _), id)| {
                ContentBlock::tool_use(
                    id.clone(),
                    "SkillTool",
                    serde_json::json!({ "skill_name": name }),
                )
            })
            .collect();

        // 追加 Ai 消息（ai_from_blocks 自动双写 tool_calls）
        state.add_message(BaseMessage::ai_from_blocks(tool_use_blocks));

        // 内容加载入口已完成 MCP 来源标注。
        for (id, (_, content)) in call_ids.iter().zip(skill_contents) {
            state.add_message(BaseMessage::tool_result(id.clone(), content));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "skill_preload_test.rs"]
mod tests;
