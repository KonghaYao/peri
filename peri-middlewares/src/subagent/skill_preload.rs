use async_trait::async_trait;
use peri_acp_types::mcp_skills::{McpSkillRegistry, SkillLookup};
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::r#trait::Middleware,
};

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
/// [Tool]  ToolResult{call_{hex}, skill_content}   ← 成功项（is_error = false）
/// [Tool]  ToolError{call_{hex}, gap_text}         ← 缺口项（is_error = true）
/// ...
/// ```
///
/// # 缺口处理（W6）
///
/// 无法预载的声明技能**不静默跳过**（plan §5.6：无法满足 preload 时报告缺口，
/// 不静默启动不完整配置），按路径分流：
///
/// - **宿主显式名单路径**（`skill_names` 非空：子代理与 workflow agent 定义中的
///   显式声明）：每个缺口注入一对「假 `SkillTool` 调用 + 失败回执」（`tool_error`，
///   `is_error = true`），与成功项同处一条 Ai 消息、按声明顺序一一配对；
/// - **主 Agent 路径**（从最后一条 Human 消息启发式提取 `/token`）：缺口零注入。
///   提取是启发式的——用户文本里的路径片段（如 `/tmp`）会命中——凭空注入失败
///   回执会制造用户从未发起的工具错误，而原始 `/token` 文本仍在 Human 消息里可见。
///
/// 缺口仍然只做报告（warn/debug 日志保留）；注入的回执文案取自
/// `crate::skills::skill_*_message` 单一派生点——与 `SkillTool`
/// （`crate::skills::tools`）同名失败串逐字一致，不新造格式。
///
/// 名单元素按**名**解析（`lookup_exact` 全名 → `lookup_by_command` `{server}:{skill}`
/// → 裸名 `lookup`）；URI 形态（`skill://…/SKILL.md`）不被接受，会落入 not-found 缺口。
///
/// 已登记副作用（验收 §6.3）：假调用与回执随 `transcript.append` 持久化，并经会话
/// 回放投影为 SkillTool 卡片——它是模型必须看到的缺口事实，同样在客户端可见。
pub struct SkillPreloadMiddleware {
    skill_names: Vec<String>,
    /// 会话级 MCP skill 远端注册表（W4b：技能的**唯一**来源；None = 未装配
    /// 技能面 → 预载不可用，缺口报告，不回落磁盘）。
    mcp_registry: Option<std::sync::Arc<McpSkillRegistry>>,
}

impl SkillPreloadMiddleware {
    /// W4b（J5）：`cwd` 形参随本地扫描一并删除——技能根解析归 workspace 实例的
    /// provider，宿主预载不再需要任何路径。
    pub fn new(skill_names: Vec<String>) -> Self {
        Self {
            skill_names,
            mcp_registry: None,
        }
    }

    /// 注入 MCP 远端技能注册表。
    pub fn with_mcp_registry(mut self, reg: Option<std::sync::Arc<McpSkillRegistry>>) -> Self {
        self.mcp_registry = reg;
        self
    }
}

/// 注入项：单个声明技能的预载结果（按声明顺序与 ToolUse 块一一配对）。
struct PreloadItem {
    /// 技能名（统一小写——与 registry 查找、既有缺口日志同口径）。
    name: String,
    /// `Ok` = 已校验正文（经 MCP 来源标注）；`Err` = 缺口回执文案。
    outcome: Result<String, String>,
}

/// registry 单名查找结果（按声明顺序逐名产出，缺口留在原位）。
///
/// W4b（F5/J5）：本地磁盘命中槽位已删除——技能没有非 MCP 来源。正文激活仍在
/// 异步路径进行（资源读取不能在 blocking 闭包里做）。
enum RegistryLookup {
    /// 命中（正文激活在闭包外统一进行）。
    ///
    /// 盒装：SkillMetadata 远大于其他变体（clippy large_enum_variant）。
    Found(Box<peri_acp_types::skills::SkillMetadata>),
    /// 跨 origin 同名命中：显式拒绝注入（回执携带候选清单）。
    Ambiguous(Vec<peri_acp_types::skills::SkillMetadata>),
    /// 未在 registry 命中。
    Missing,
}

/// 追加「Ai[ToolUse{SkillTool}…] + 逐条 Tool 结果」注入序列（成功与缺口同一批）。
///
/// `items` 顺序 = 声明顺序；ToolUse 块与结果消息按序一一配对：`Ok` ⇒
/// `tool_result`（`is_error = false`），`Err` ⇒ `tool_error`（`is_error = true`）。
/// 调用方保证 `items` 非空。
fn inject_skill_tool_sequence(
    state: &mut dyn hook_state::BeforeAgentState,
    items: Vec<PreloadItem>,
) {
    // Generate tool_call_ids: call_{uuid hex without hyphens, 32 chars}
    let call_ids: Vec<String> = (0..items.len())
        .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
        .collect();

    // 构造 Ai 消息的 ToolUse ContentBlock 列表（fake SkillTool 工具调用）
    let tool_use_blocks: Vec<ContentBlock> = items
        .iter()
        .zip(call_ids.iter())
        .map(|(item, id)| {
            ContentBlock::tool_use(
                id.clone(),
                "SkillTool",
                serde_json::json!({ "skill_name": item.name }),
            )
        })
        .collect();

    // 追加 Ai 消息（ai_from_blocks 自动双写 tool_calls）
    state.add_message(BaseMessage::ai_from_blocks(tool_use_blocks));

    // 成功正文在内容加载入口已完成 MCP 来源标注；缺口项为 `is_error` 回执。
    for (id, item) in call_ids.iter().zip(items) {
        state.add_message(match item.outcome {
            Ok(content) => BaseMessage::tool_result(id.clone(), content),
            Err(text) => BaseMessage::tool_error(id.clone(), text),
        });
    }
}

#[async_trait]
impl Middleware for SkillPreloadMiddleware {
    fn name(&self) -> &str {
        "SkillPreloadMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        // 确定要预加载的 skill 名称列表；`explicit_list_path` 决定缺口的注入语义
        // （见 struct 文档「缺口处理」）。
        let explicit_list_path = !self.skill_names.is_empty();
        let skill_names = if explicit_list_path {
            // 宿主显式名单路径（子代理 / workflow）：使用构造时传入的列表
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

        // W4b（F5/J5）：预载只按名查 MCP registry——本地磁盘兜底扫描已删除。
        // 未装配 registry（print/遗留装配）或未命中 → 缺口报告（warn），
        // 不回落磁盘、不静默假装成功；显式名单路径（W6：子代理 / workflow）在此
        // 仍产出缺口配对。
        let Some(registry) = self.mcp_registry.clone() else {
            tracing::warn!(
                skills = ?skill_names,
                "SkillPreloadMiddleware: MCP skill registry 未装配，技能预载不可用（缺口，不回落磁盘）"
            );
            if explicit_list_path {
                let items = skill_names
                    .into_iter()
                    .map(|name| {
                        let name = name.to_lowercase();
                        PreloadItem {
                            name: name.clone(),
                            outcome: Err(crate::skills::skill_registry_unwired_message(&name)),
                        }
                    })
                    .collect();
                inject_skill_tool_sequence(state, items);
            }
            return Ok(());
        };

        // 一批预载共享一次 registry 读取（registry 内部锁内取快照）。按输入
        // 顺序逐名产出查找结果（命中/歧义/未命中），缺口留在原位。
        // 闭包只借一份 registry 快照；激活阶段继续用外层句柄（Arc 克隆廉价）。
        let lookup_registry = std::sync::Arc::clone(&registry);
        let lookups: Vec<(String, RegistryLookup)> = tokio::task::spawn_blocking(move || {
            skill_names
                .into_iter()
                .map(|name| {
                    let name = name.to_lowercase();
                    // 三形态与命令面同源：全名/别名（`lookup_exact`）→
                    // `{server}:{skill}` 命令形态（`lookup_by_command`，覆盖 plugin
                    // 多冒号 server 的末段匹配）→ 裸名 `{skill}`（`lookup`，跨 origin
                    // 命中返回 Ambiguous，由调用方显式拒绝而不是静默取首个）。
                    let lookup = match lookup_registry.lookup_exact(&name) {
                        SkillLookup::Missing => match lookup_registry.lookup_by_command(&name) {
                            SkillLookup::Missing => lookup_registry.lookup(&name),
                            other => other,
                        },
                        other => other,
                    };
                    // 用户文本的旧 /server:skill 形态不再是系统 skill 命令。
                    // 仅自动提取路径限制；子代理/workflow 的显式名单保持原语义。
                    let lookup = if !explicit_list_path && name.contains(':') {
                        match lookup {
                            SkillLookup::Found(ref meta)
                                if meta.origin.as_ref().is_some_and(
                                    |peri_acp_types::skills::SkillOrigin::Mcp {
                                         server, ..
                                     }| {
                                        lookup_registry.is_system_origin(server)
                                    },
                                ) =>
                            {
                                SkillLookup::Missing
                            }
                            other => other,
                        }
                    } else {
                        lookup
                    };
                    let outcome = match lookup {
                        SkillLookup::Found(meta) => RegistryLookup::Found(meta),
                        SkillLookup::Ambiguous(candidates) => RegistryLookup::Ambiguous(candidates),
                        SkillLookup::Missing => RegistryLookup::Missing,
                    };
                    (name, outcome)
                })
                .collect()
        })
        .await
        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
            middleware: "SkillPreloadMiddleware".to_string(),
            reason: format!("spawn_blocking 失败: {e}"),
        })?;

        // 逐名处置（保持声明顺序）：命中项走统一 activation（resources/read +
        // digest/frontmatter 校验；stale 经 skills/get 刷新一次，见
        // `skill_activation`），失败不回退磁盘或发现期缓存。
        let mut items: Vec<PreloadItem> = Vec::with_capacity(lookups.len());
        for (name, lookup) in lookups {
            match lookup {
                RegistryLookup::Found(meta) => {
                    match crate::mcp::skill_activation::activate(&registry, meta.as_ref(), None)
                        .await
                    {
                        // 内容加载入口已完成 MCP 来源标注。
                        Ok(content) => {
                            let content =
                                crate::skills::annotate_mcp_content(meta.as_ref(), &content);
                            items.push(PreloadItem {
                                name,
                                outcome: Ok(content),
                            });
                        }
                        Err(error) => {
                            tracing::debug!(
                                skill = %name,
                                reason = error.reason(),
                                "MCP skill 预加载激活失败，跳过注入"
                            );
                            items.push(PreloadItem {
                                outcome: Err(crate::skills::skill_activation_failed_message(
                                    &name,
                                    error.reason(),
                                )),
                                name,
                            });
                        }
                    }
                }
                RegistryLookup::Ambiguous(candidates) => {
                    // 显式拒绝（不注入正文、不回落本地），候选写入日志供消歧；
                    // 回执文案与 `SkillTool` 的歧义错误同源（`crate::skills`
                    // 单一派生点，含 candidate_list）。
                    let list = crate::mcp::skill_discovery::candidate_list(&candidates);
                    tracing::warn!(
                        skill = %name,
                        candidates = %list,
                        "MCP skill 预加载命中多个 origin，显式拒绝注入（请用完整名消歧）"
                    );
                    items.push(PreloadItem {
                        outcome: Err(crate::skills::skill_ambiguous_message(&name, &candidates)),
                        name,
                    });
                }
                RegistryLookup::Missing => {
                    tracing::warn!(
                        skill = %name,
                        "SkillPreloadMiddleware: skill 未在 MCP registry 中命中，跳过预载（缺口）"
                    );
                    items.push(PreloadItem {
                        outcome: Err(crate::skills::skill_not_found_message(&name)),
                        name,
                    });
                }
            }
        }

        // 主 Agent 路径：缺口零注入（启发式 token 提取，见 struct 文档）；
        // 成功项照旧注入。
        if !explicit_list_path {
            items.retain(|item| item.outcome.is_ok());
        }
        if items.is_empty() {
            return Ok(());
        }

        inject_skill_tool_sequence(state, items);

        Ok(())
    }
}

#[cfg(test)]
#[path = "skill_preload_test.rs"]
mod tests;
