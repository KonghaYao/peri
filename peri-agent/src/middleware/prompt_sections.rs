//! 段落持有者接口（波 4 演进基础设施，设计 §3.1.1 拆分持有契约 2/3/4）。
//!
//! Middleware 经 [`Middleware::prompt_sections`]（`trait.rs`）声明其持有的
//! 系统提示词段落（内容载体），装配期经 [`MiddlewareChain::collect_prompt_sections`]
//! （`chain.rs`）收集，渲染面（`peri-acp` 的 `PromptTemplate`）按"位置属性 +
//! 段内序号"排序装配（**不依赖 middleware 链序**，契约 2）。
//!
//! 语义边界（设计 §3.5）：middleware 仅作内容载体——段落渲染仍走
//! `PromptTemplate` 段落装配渲染，本接口**不是** `prompt_contribution`
//! （`before_agent` 后按每个 `ModelRequest` 读取的动态后缀）的替代通道；
//! `prompt_contribution` 语义不变。
//!
//! 契约 3（gate 原子迁移，C2/C3 落地）：段落 gate = "持有该段的 middleware
//! 是否在链上"。收集机制天然隐含此判定——能收集到段落即持有者已装配
//! （gate 开启）；[`project_enabled_sections`] 是同一判定的显式投影（映射表
//! `peri_acp_types::meta_harness::SECTION_HOLDER_MIDDLEWARE`），段落可见性由
//! 持有 middleware 的装配事实决定，不依赖独立的 feature gate。
//!
//! 契约 4（运行时缺失防御）：middleware 在链上但未提供段落（默认空列表）
//! = 跳过渲染不 fail；渲染面物化时过滤空内容段落（见 `PromptTemplate::new`）。

use std::collections::{HashMap, HashSet};

use peri_acp_types::meta_harness::SECTION_HOLDER_MIDDLEWARE;

/// 段落位置属性（契约 2）：boundary 前缓存区 / boundary 后非缓存区。
///
/// 与渲染生成段（Persona / Language）的相对位置由渲染面固定（现状顺序
/// 语义保持）；本枚举只承担"段落归属哪个区"的显式标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromptSectionZone {
    /// boundary 前：Anthropic 缓存命中区（现状 01-06）。
    Cached,
    /// boundary 后：非缓存区（现状 07/14、gated 段落、收集段）。
    Uncached,
}

/// 段落内容来源（持有者侧；覆盖合并发生在 `PromptTemplate` 构造期）。
#[derive(Debug, Clone)]
pub enum PromptSectionContent {
    /// 编译期嵌入的静态文本（`include_str!`，零拷贝；迁移时文件可留在
    /// `prompts/sections/` 由持有 middleware `include_str!`，内容不复制）。
    Builtin(&'static str),
    /// 运行时生成的动态文本（middleware 按装配事实生成，如 10_hitl 的
    /// sensitive 列表、13_skills 的协议细节）。
    Dynamic(String),
}

impl PromptSectionContent {
    /// 渲染用文本视图
    pub fn as_str(&self) -> &str {
        match self {
            Self::Builtin(s) => s,
            Self::Dynamic(s) => s,
        }
    }
}

/// middleware 持有的系统提示词段落声明（内容载体；契约 2 位置属性 + 段内序号）。
///
/// - `id`：段落 ID（`prompts/sections/` 文件名去 `.md`；MetaHarness 段落覆盖
///   与持有权迁移按 ID 定位）。
/// - `zone` + `order`：装配期排序依据（契约 2：不依赖 middleware 链序）。
/// - `content`：段落内容来源（静态零拷贝 / 动态生成）。
#[derive(Debug, Clone)]
pub struct PromptSection {
    pub id: &'static str,
    pub zone: PromptSectionZone,
    pub order: u16,
    pub content: PromptSectionContent,
}

impl PromptSection {
    /// 构造静态段落声明（`include_str!` 内容零拷贝）。
    pub fn builtin(
        id: &'static str,
        zone: PromptSectionZone,
        order: u16,
        content: &'static str,
    ) -> Self {
        Self {
            id,
            zone,
            order,
            content: PromptSectionContent::Builtin(content),
        }
    }

    /// 构造动态段落声明（middleware 按装配事实生成内容）。
    pub fn dynamic(id: &'static str, zone: PromptSectionZone, order: u16, content: String) -> Self {
        Self {
            id,
            zone,
            order,
            content: PromptSectionContent::Dynamic(content),
        }
    }
}

/// 装配期段落 gate 投影（契约 3）：链上 middleware 名集合 → gate 开启的段落
/// ID 集合。
///
/// 判定规则：段落 gate = 持有该段的 middleware 是否在链上，映射表为
/// [`SECTION_HOLDER_MIDDLEWARE`]（`peri-acp-types` 契约层）。本函数是收集机制
/// 的显式视图——从链上能收集到段落即持有者已装配，段落可见性由持有
/// middleware 决定；收集结果与投影的一致性由
/// `assembly_test.rs` 的 `chain_collected_gated_sections_match_projection`
/// 锁定）。
///
/// 纯函数（输入名集合而非链实例），便于测试与装配面复用。
pub fn project_enabled_sections(chain_middleware_names: &HashSet<&str>) -> HashSet<&'static str> {
    SECTION_HOLDER_MIDDLEWARE
        .iter()
        .filter(|(_, holder)| chain_middleware_names.contains(*holder))
        .map(|(id, _)| *id)
        .collect()
}

/// 段落装配冲突（M11 守护）：跨持有者的 section id 或 (zone, order) 不唯一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionLayoutError {
    /// 同一 section id 被声明两次（多个持有者或同一持有者重复声明）。
    DuplicateId { id: &'static str },
    /// 同一 (zone, order) 被两个段落占用——渲染顺序不再唯一。
    DuplicateSlot {
        zone: PromptSectionZone,
        order: u16,
        first: &'static str,
        second: &'static str,
    },
}

impl std::fmt::Display for SectionLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId { id } => {
                write!(
                    f,
                    "section id '{id}' 被重复声明（来源：多个持有者或重复声明）"
                )
            }
            Self::DuplicateSlot {
                zone,
                order,
                first,
                second,
            } => write!(
                f,
                "段落 '{first}' 与 '{second}' 占用同一位置 (zone={zone:?}, order={order})"
            ),
        }
    }
}

impl std::error::Error for SectionLayoutError {}

/// 跨持有者段落布局校验（M11）：section id 与 (zone, order) 必须唯一。
///
/// 冲突即装配错误——渲染顺序不再是「同序号按声明顺序」的兜底语义，而是在构造
/// 期显式失败并指出来源段落；渲染面（`PromptTemplate::new`）在构造期调用本
/// 函数并对错误显式失败。
pub fn validate_section_layout(sections: &[PromptSection]) -> Result<(), SectionLayoutError> {
    let mut ids: HashSet<&'static str> = HashSet::with_capacity(sections.len());
    let mut slots: HashMap<(PromptSectionZone, u16), &'static str> =
        HashMap::with_capacity(sections.len());
    for section in sections {
        if !ids.insert(section.id) {
            return Err(SectionLayoutError::DuplicateId { id: section.id });
        }
        if let Some(first) = slots.insert((section.zone, section.order), section.id) {
            return Err(SectionLayoutError::DuplicateSlot {
                zone: section.zone,
                order: section.order,
                first,
                second: section.id,
            });
        }
    }
    Ok(())
}

/// 执行面段落能力事实（H2/M11）。
///
/// 描述一个执行面（主链 / 子链 / workflow 链）**实际具备**哪些能力段落。
/// 事实来源是真实装配结果——链收集到的段落声明（
/// [`SectionCapabilities::from_sections`]），或由装配条件投影的等价事实；
/// 渲染面按同一策略消费，不维护第二份按执行类型硬编码的排除表。
///
/// 除持有者是否存在，能力事实还承担**有效模式**：例如 `PermissionMiddleware`
/// 的 `disabled()` 实例（无审批通道）不声明审批段——「存在不等于需要审批
/// 说明」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionCapabilities {
    /// 基础段持有者（01-06 / 07_runtime / persona）。
    pub base_prompt: bool,
    /// 语言段持有者。
    pub language: bool,
    /// 审批段（10_hitl）：持有者存在且审批有效。
    pub approval: bool,
    /// 提问段（12_ask_user）：持有者装配。
    pub ask_user: bool,
    /// 子代理段（11_subagent）：持有者装配。
    pub subagent: bool,
    /// 技能段（13_skills）：持有者装配。
    pub skills: bool,
}

impl SectionCapabilities {
    /// 从实际装配的段落声明派生能力事实（链收集结果即装配事实）。
    pub fn from_sections(sections: &[PromptSection]) -> Self {
        let has = |id: &str| sections.iter().any(|section| section.id == id);
        Self {
            base_prompt: has("01_intro") || has("07_runtime") || has("persona"),
            language: has("language"),
            approval: has("10_hitl"),
            ask_user: has("12_ask_user"),
            subagent: has("11_subagent"),
            skills: has("13_skills"),
        }
    }

    /// 基础段是否可用（子链/workflow 链无基础段持有者时由冻结模板继承，
    /// 该标志仍表达「本执行面继承的基础段决策」）。
    pub fn with_inherited_base(mut self, base_prompt: bool, language: bool) -> Self {
        self.base_prompt = base_prompt;
        self.language = language;
        self
    }
}
