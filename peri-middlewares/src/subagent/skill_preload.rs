use async_trait::async_trait;
use peri_acp_types::mcp_skills::{McpSkillRegistry, SkillLookup};
use peri_acp_types::skills::SkillOrigin;
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

/// 一批预载最多实际加载的 skill 条数（超出的显式条目产出缺口回执）。
const MAX_PRELOAD_ITEMS: usize = 16;
/// 单项正文（含来源标注）的 UTF-8 字节预算。
const MAX_PRELOAD_ITEM_BYTES: usize = 64 * 1024;
/// 整批正文（含来源标注）的共享字节预算。
const MAX_PRELOAD_BATCH_BYTES: usize = 128 * 1024;

/// SkillPreloadMiddleware - 将指定 skill 全文以 fake SkillTool 调用注入到 agent state
///
/// 输入生命周期（H7）：
/// - **宿主显式名单**（子代理 / workflow agent 定义里的 `skill_names`）：只在
///   `before_agent` 初始化执行一次，不搬到每个输入 hook 重复加载；
/// - **主 Agent 启发式路径**（用户消息里的 `/token`）：在 `before_input` 上按
///   **本批输入身份**（`input_message_ids`）提取，首批由 `run_before_agent` 的
///   交错调用覆盖，同 loop 的中途批次（steering / SDK 追加）由后续
///   `run_before_input` 覆盖；不扫描历史最后一条 Human。
///
/// 注入的消息序列把 skill 正文以 Ai[ToolUse{SkillTool}] → Tool[ToolResult]
/// 形式追加到用户消息之后（executor 在输入准备前已将用户消息 `add_message` 到
/// state），使 LLM 从第一轮推理就能看到完整 skill 内容。
///
/// 注入的 ToolUse 名为 `SkillTool`（与会话中真实注册的统一 skill 加载协议一致，
/// 见 D3：`Skill(skill, args)` 已移除，模型可见协议只剩 `SkillTool(skill_name)` +
/// `DiscoverSkillsTool`），input 为 `{"skill_name": <名称>}`。
///
/// 使用追加而非前插，确保工具调用出现在用户消息之后，不影响 Anthropic messages
/// 数组的 prompt cache（cache_control 在第一条 user 消息上）。
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
///   `is_error = true`），与成功项同处一条 Ai 消息、按声明顺序一一配对——**条数 /
///   字节预算超限的条目同样配对回执**，不加载截断正文冒充完整指令；
/// - **主 Agent 路径**（从本批输入启发式提取 `/token`）：缺口零注入。
///   提取是启发式的——用户文本里的路径片段（如 `/tmp`）会命中——凭空注入失败
///   回执会制造用户从未发起的工具错误，而原始 `/token` 文本仍在 Human 消息里可见。
///   **例外**：已识别（registry 命中）但被预算挡下的技能必须显式提示，不能假装
///   什么都没声明。
///
/// 缺口仍然只做报告（warn/debug 日志保留）；注入的回执文案取自
/// `crate::skills::skill_*_message` 单一派生点——与 `SkillTool`
/// （`crate::skills::tools`）同名失败串逐字一致，不新造格式。
///
/// 名单元素按**名**解析（`lookup_exact` 全名 → `lookup_by_command` `{server}:{skill}`
/// → 裸名 `lookup`）；URI 形态（`skill://…/SKILL.md`）不被接受，会落入 not-found 缺口。
///
/// # 批内去重与预算（M8）
///
/// 按 canonical 身份（`SkillOrigin::Mcp { server, uri }`）批内去重，保持首次出现
/// 顺序；同一 skill 的重复声明 / 别名只读一次、只注入一次（debug 记录）。条数、
/// 单项与整批字节预算超限时不读正文（预算耗尽后不再发起读取），产出配对缺口回执。
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
    outcome: PreloadOutcome,
}

/// 单项处置结果。
enum PreloadOutcome {
    /// 已校验正文（经 MCP 来源标注）。
    Loaded(String),
    /// 缺口回执文案；`report_in_heuristic` 决定启发式路径是否也注入。
    ///
    /// 启发式路径的既有规则是「不制造未知技能错误」（not-found / 歧义 / 激活
    /// 失败零注入）；**预算缺口**是已识别技能的显式提示，两条路径都注入。
    Gap {
        text: String,
        report_in_heuristic: bool,
    },
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

/// canonical skill 身份（来源 + URI）：批内去重的事实源。
fn canonical_identity(meta: &peri_acp_types::skills::SkillMetadata) -> (String, String) {
    match meta.origin.as_ref() {
        Some(SkillOrigin::Mcp { server, uri }) => (server.clone(), uri.clone()),
        // 非 MCP 来源（规范上已退出）：用 path 作身份，仍保持批内去重语义。
        None => (String::new(), meta.path.to_string_lossy().into_owned()),
    }
}

/// 一批预载的共享预算（条数与正文 UTF-8 字节；批内所有条目共享）。
struct PreloadBudget {
    loaded_items: usize,
    remaining_bytes: usize,
}

impl PreloadBudget {
    fn new() -> Self {
        Self {
            loaded_items: 0,
            remaining_bytes: MAX_PRELOAD_BATCH_BYTES,
        }
    }

    fn items_exhausted(&self) -> bool {
        self.loaded_items >= MAX_PRELOAD_ITEMS
    }

    fn bytes_exhausted(&self) -> bool {
        self.remaining_bytes == 0
    }

    /// 本项可用的字节上限（单项预算与批内剩余取小）。
    fn item_allowance(&self) -> usize {
        MAX_PRELOAD_ITEM_BYTES.min(self.remaining_bytes)
    }

    fn spend(&mut self, bytes: usize) {
        self.loaded_items += 1;
        self.remaining_bytes = self.remaining_bytes.saturating_sub(bytes);
    }

    /// 单项超限后批内预算不再可用：后续条目不读正文，直接产出预算缺口。
    fn exhaust_bytes(&mut self) {
        self.remaining_bytes = 0;
    }
}

/// 追加「Ai[ToolUse{SkillTool}…] + 逐条 Tool 结果」注入序列（成功与缺口同一批）。
///
/// `items` 顺序 = 声明顺序；ToolUse 块与结果消息按序一一配对：`Loaded` ⇒
/// `tool_result`（`is_error = false`），`Gap` ⇒ `tool_error`（`is_error = true`）。
/// 调用方保证 `items` 非空。
fn inject_skill_tool_sequence<S: hook_state::BeforeInputState + ?Sized>(
    state: &mut S,
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
    state.append_input_message(BaseMessage::ai_from_blocks(tool_use_blocks));

    // 成功正文在内容加载入口已完成 MCP 来源标注；缺口项为 `is_error` 回执。
    for (id, item) in call_ids.iter().zip(items) {
        state.append_input_message(match item.outcome {
            PreloadOutcome::Loaded(content) => BaseMessage::tool_result(id.clone(), content),
            PreloadOutcome::Gap { text, .. } => BaseMessage::tool_error(id.clone(), text),
        });
    }
}

/// 本批输入里出现的 `/token`（保持出现顺序，逐名去重；空批次返回空）。
fn heuristic_skill_names(state: &dyn hook_state::BeforeInputState) -> Vec<String> {
    let Some(ids) = state.input_message_ids() else {
        tracing::debug!(
            "SkillPreloadMiddleware: 本次输入没有批次身份（legacy 适配器），跳过 /token 提取"
        );
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for message in state.messages().iter().filter(|message| {
        matches!(message, BaseMessage::Human { .. }) && ids.contains(&message.id())
    }) {
        for name in extract_skill_names_from_text(&message.content()) {
            if !names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
            {
                names.push(name);
            }
        }
    }
    names
}

impl SkillPreloadMiddleware {
    /// 预载一批技能名；`explicit_list_path` 决定缺口注入语义（见 struct 文档）。
    ///
    /// 通用参数化在 `BeforeInputState` 上：`before_agent` 的显式名单路径与
    /// `before_input` 的启发式路径共用同一实现（trait 对象不做向上转型）。
    async fn preload<S: hook_state::BeforeInputState + ?Sized>(
        &self,
        state: &mut S,
        skill_names: Vec<String>,
        explicit_list_path: bool,
    ) -> AgentResult<()> {
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
                            outcome: PreloadOutcome::Gap {
                                text: crate::skills::skill_registry_unwired_message(&name),
                                report_in_heuristic: false,
                            },
                            name,
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
        let mut budget = PreloadBudget::new();
        let mut seen_identities: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for (name, lookup) in lookups {
            match lookup {
                RegistryLookup::Found(meta) => {
                    let identity = canonical_identity(meta.as_ref());
                    if !seen_identities.insert(identity.clone()) {
                        // 同批内同一 canonical skill（重复名 / 别名）：只读一次、
                        // 只注入一次，保持首次出现位置；不重复占用正文预算。
                        tracing::debug!(
                            skill = %name,
                            server = %identity.0,
                            uri = %identity.1,
                            "同批已加载该 canonical skill，跳过重复预载"
                        );
                        continue;
                    }
                    // 预算闸门先于读取：条数超限或批内字节耗尽时**不读正文**，
                    // 直接产出显式缺口回执（不加载截断正文冒充完整指令）。
                    if budget.items_exhausted() {
                        tracing::warn!(
                            skill = %name,
                            limit = MAX_PRELOAD_ITEMS,
                            "技能预载条数超过批预算，未加载（配对缺口回执）"
                        );
                        items.push(PreloadItem {
                            outcome: PreloadOutcome::Gap {
                                text: crate::skills::skill_preload_budget_message(
                                    &name,
                                    &format!("batch item limit {MAX_PRELOAD_ITEMS} reached"),
                                ),
                                report_in_heuristic: true,
                            },
                            name,
                        });
                        continue;
                    }
                    if budget.bytes_exhausted() {
                        tracing::warn!(
                            skill = %name,
                            limit = MAX_PRELOAD_BATCH_BYTES,
                            "技能预载字节预算已用尽，未加载（配对缺口回执）"
                        );
                        items.push(PreloadItem {
                            outcome: PreloadOutcome::Gap {
                                text: crate::skills::skill_preload_budget_message(
                                    &name,
                                    &format!(
                                        "batch byte budget {MAX_PRELOAD_BATCH_BYTES} exhausted"
                                    ),
                                ),
                                report_in_heuristic: true,
                            },
                            name,
                        });
                        continue;
                    }
                    match crate::mcp::skill_activation::activate(&registry, meta.as_ref(), None)
                        .await
                    {
                        // 内容加载入口已完成 MCP 来源标注。
                        Ok(content) => {
                            let content =
                                crate::skills::annotate_mcp_content(meta.as_ref(), &content);
                            let allowance = budget.item_allowance();
                            if content.len() > allowance {
                                tracing::warn!(
                                    skill = %name,
                                    bytes = content.len(),
                                    allowance,
                                    "技能正文超过预载字节预算，不加载截断正文（配对缺口回执）"
                                );
                                budget.exhaust_bytes();
                                items.push(PreloadItem {
                                    outcome: PreloadOutcome::Gap {
                                        text: crate::skills::skill_preload_budget_message(
                                            &name,
                                            &format!("content exceeds byte budget {allowance}"),
                                        ),
                                        report_in_heuristic: true,
                                    },
                                    name,
                                });
                                continue;
                            }
                            budget.spend(content.len());
                            items.push(PreloadItem {
                                name,
                                outcome: PreloadOutcome::Loaded(content),
                            });
                        }
                        Err(error) => {
                            tracing::debug!(
                                skill = %name,
                                reason = error.reason(),
                                "MCP skill 预加载激活失败，跳过注入"
                            );
                            items.push(PreloadItem {
                                outcome: PreloadOutcome::Gap {
                                    text: crate::skills::skill_activation_failed_message(
                                        &name,
                                        error.reason(),
                                    ),
                                    report_in_heuristic: false,
                                },
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
                        outcome: PreloadOutcome::Gap {
                            text: crate::skills::skill_ambiguous_message(&name, &candidates),
                            report_in_heuristic: false,
                        },
                        name,
                    });
                }
                RegistryLookup::Missing => {
                    tracing::warn!(
                        skill = %name,
                        "SkillPreloadMiddleware: skill 未在 MCP registry 中命中，跳过预载（缺口）"
                    );
                    items.push(PreloadItem {
                        outcome: PreloadOutcome::Gap {
                            text: crate::skills::skill_not_found_message(&name),
                            report_in_heuristic: false,
                        },
                        name,
                    });
                }
            }
        }

        // 主 Agent 路径：未识别条目零注入（启发式 token 提取，见 struct 文档）；
        // 已识别但被预算挡下的条目仍显式提示，成功项照旧注入。
        if !explicit_list_path {
            items.retain(|item| match &item.outcome {
                PreloadOutcome::Loaded(_) => true,
                PreloadOutcome::Gap {
                    report_in_heuristic,
                    ..
                } => *report_in_heuristic,
            });
        }
        if items.is_empty() {
            return Ok(());
        }

        inject_skill_tool_sequence(state, items);

        Ok(())
    }
}

#[async_trait]
impl Middleware for SkillPreloadMiddleware {
    fn name(&self) -> &str {
        "SkillPreloadMiddleware"
    }

    /// 宿主显式名单路径（子代理 / workflow 定义）——**只在初始化执行一次**，
    /// 不随每个输入批次重复加载（H7）。主 Agent 的 `/token` 启发式路径在
    /// `before_input` 上处理，这里直接返回。
    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        if self.skill_names.is_empty() {
            return Ok(());
        }
        self.preload(state, self.skill_names.clone(), true).await
    }

    /// 主 Agent 启发式路径：消费**本批输入身份**，不扫描历史最后一条 Human；
    /// 空批次不注入。显式名单由 `before_agent` 负责，这里不重复加载。
    async fn before_input(&self, state: &mut dyn hook_state::BeforeInputState) -> AgentResult<()> {
        if !self.skill_names.is_empty() {
            return Ok(());
        }
        let names = heuristic_skill_names(state);
        self.preload(state, names, false).await
    }
}

#[cfg(test)]
#[path = "skill_preload_test.rs"]
mod tests;
