//! Owner-declared sections collected at assembly. Request-time contributions
//! are read from the active chain when the model request is built.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Debug, Formatter};

pub use peri_acp_types::meta_harness::PromptSectionZone;
use peri_acp_types::meta_harness::{ChainSlot, SectionSpec};

/// 段落内容来源（持有者侧；覆盖合并发生在 `PromptTemplate` 构造期）。
#[derive(Debug, Clone)]
pub enum PromptSectionContent {
    /// 编译期嵌入的静态文本（`include_str!`，零拷贝；迁移时文件可留在
    /// `prompts/sections/` 由持有 middleware `include_str!`，内容不复制）。
    Builtin(&'static str),
    /// 运行时生成的动态文本（middleware 按装配事实生成，如技能发现协议或已冻结的语言选择）。
    Dynamic(String),
}

impl PromptSectionContent {
    /// 渲染用文本视图
    #[must_use]
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
    pub owner: Option<ChainSlot>,
    /// Makes the session agent catalog available to uncached templates.
    pub provides_agent_catalog: bool,
    /// A restricted binding may require its owner-authored instructions.
    pub accepts_override: bool,
    pub id: &'static str,
    pub zone: PromptSectionZone,
    pub order: u16,
    pub content: PromptSectionContent,
}

impl PromptSection {
    pub fn with_agent_catalog(mut self) -> Self {
        self.provides_agent_catalog = true;
        self
    }

    pub fn without_override(mut self) -> Self {
        self.accepts_override = false;
        self
    }

    pub fn declared(spec: SectionSpec, content: PromptSectionContent) -> Self {
        Self {
            owner: Some(spec.owner),
            provides_agent_catalog: false,
            accepts_override: true,
            id: spec.id,
            zone: spec.zone,
            order: spec.order,
            content,
        }
    }

    /// 构造静态段落声明（`include_str!` 内容零拷贝）。
    #[must_use]
    pub fn builtin(
        id: &'static str,
        zone: PromptSectionZone,
        order: u16,
        content: &'static str,
    ) -> Self {
        Self {
            owner: None,
            provides_agent_catalog: false,
            accepts_override: true,
            id,
            zone,
            order,
            content: PromptSectionContent::Builtin(content),
        }
    }

    /// 构造动态段落声明（middleware 按装配事实生成内容）。
    #[must_use]
    pub fn dynamic(id: &'static str, zone: PromptSectionZone, order: u16, content: String) -> Self {
        Self {
            owner: None,
            provides_agent_catalog: false,
            accepts_override: true,
            id,
            zone,
            order,
            content: PromptSectionContent::Dynamic(content),
        }
    }
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
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
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
