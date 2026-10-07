//! 段落覆盖准入校验（L3）与 Cached 静态约束（M11）。
//!
//! 覆盖正文在**冻结准入点**统一校验：空内容、单段/总字节预算、reserved cache
//! boundary token、未知 `{{name}}` 占位符；`Cached` 段额外要求纯静态（不得含
//! 任何模板占位符，动态环境/能力值会破坏「Cached = 静态前缀」契约）。非法覆盖
//! **拒绝应用并保留内置段**，按段落 + 错误类别记录结构化诊断（不输出正文）。
//!
//! 旧 snapshot 的正文不被自动重写：解码后的旧覆盖在模板构造期再次经过同一
//! 校验，非法项同样按「拒绝应用、保留内置」降级并 warn——旧会话仍可读，不伪
//! 造、不静默剥离。
//!
//! 未知 `{{name}}` 视为模板错误；需要字面花括号时用转义 `\{{` / `\}}`
//! （见 [`super::PromptTemplate::render`]）。

use std::collections::HashMap;

use peri_acp_types::meta_harness::MetaHarnessState;
use peri_acp_types::model::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
use peri_agent::middleware::{PromptSection, PromptSectionZone};

use super::{placeholder_tokens, KNOWN_PLACEHOLDERS};

/// 单段覆盖文本的 UTF-8 字节上限。
///
/// 准入预算而非性能结论：单段提示词远小于此值，超限说明覆盖文本不是段落正文
/// （例如误粘贴整份文档）；上限保证冻结快照与每次渲染的字节量有界。
pub(crate) const MAX_SECTION_OVERRIDE_BYTES: usize = 16 * 1024;
/// 全部覆盖文本累计 UTF-8 字节上限（单段上限之外的整批约束）。
pub(crate) const MAX_TOTAL_OVERRIDE_BYTES: usize = 64 * 1024;
/// 诊断中展示的占位符样本最大长度（不输出疑似正文）。
const DIAGNOSTIC_TOKEN_MAX_CHARS: usize = 32;

/// 覆盖被拒绝的原因（结构化诊断：段落 + 错误类别；文本本身不进诊断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverrideRejection {
    /// 空内容（空白也算空）：显式拒绝，保留内置段。
    Empty,
    /// 覆盖目标段落不在本会话装配面（键存在但无段可覆盖）。
    SectionAbsent,
    /// 单段超出字节预算。
    TooLarge { bytes: usize, limit: usize },
    /// 整批累计超出字节预算。
    TotalBudgetExceeded { bytes: usize, limit: usize },
    /// 含 reserved cache boundary token（跨 String handoff 的唯一控制字）。
    ReservedBoundaryToken,
    /// 未知或未闭合的 `{{name}}`（模板错误；字面量用 `\{{` 转义）。
    UnknownPlaceholder(String),
    /// Cached 段含模板占位符（缓存区只接收纯静态模板）。
    CachedDynamicPlaceholder(String),
}

impl OverrideRejection {
    /// 诊断类别（结构化字段值：稳定、无正文）。
    pub(crate) fn category(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::SectionAbsent => "section-absent",
            Self::TooLarge { .. } => "single-section-budget",
            Self::TotalBudgetExceeded { .. } => "total-budget",
            Self::ReservedBoundaryToken => "reserved-boundary-token",
            Self::UnknownPlaceholder(_) => "unknown-placeholder",
            Self::CachedDynamicPlaceholder(_) => "cached-dynamic-placeholder",
        }
    }

    /// 诊断细节（有界，不含段落正文）。
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Empty => "空覆盖内容".to_string(),
            Self::SectionAbsent => "目标段落未装配（覆盖无效果，拒绝持久化）".to_string(),
            Self::TooLarge { bytes, limit } => format!("{bytes} bytes > 单段上限 {limit}"),
            Self::TotalBudgetExceeded { bytes, limit } => {
                format!("{bytes} bytes > 总上限 {limit}")
            }
            Self::ReservedBoundaryToken => "含 reserved cache boundary token".to_string(),
            Self::UnknownPlaceholder(token) => format!("未知占位符 {token}"),
            Self::CachedDynamicPlaceholder(token) => {
                format!("Cached 段含动态占位符 {token}")
            }
        }
    }
}

/// 校验单段覆盖文本（预算 / reserved token / 占位符 / Cached 静态约束）。
pub(crate) fn validate_section_override(
    zone: PromptSectionZone,
    text: &str,
) -> Result<(), OverrideRejection> {
    if text.trim().is_empty() {
        return Err(OverrideRejection::Empty);
    }
    if text.len() > MAX_SECTION_OVERRIDE_BYTES {
        return Err(OverrideRejection::TooLarge {
            bytes: text.len(),
            limit: MAX_SECTION_OVERRIDE_BYTES,
        });
    }
    if text.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY) {
        return Err(OverrideRejection::ReservedBoundaryToken);
    }
    for token in placeholder_tokens(text) {
        let sample = bounded_token(&token);
        if zone == PromptSectionZone::Cached {
            return Err(OverrideRejection::CachedDynamicPlaceholder(sample));
        }
        if !KNOWN_PLACEHOLDERS.contains(&token.as_str()) {
            return Err(OverrideRejection::UnknownPlaceholder(sample));
        }
    }
    Ok(())
}

/// 冻结准入：按收集到的段落（id → zone）校验全部覆盖，非法项拒绝应用。
///
/// 返回被拒绝的 `(section id, 原因)` 列表（确定性顺序：按 id 排序，见
/// ARC-SERIAL-001）；合法覆盖留在 `state.section_overrides` 并计入总预算。
pub(crate) fn sanitize_section_overrides(
    state: &mut MetaHarnessState,
    sections: &[PromptSection],
) -> Vec<(String, OverrideRejection)> {
    let zones: HashMap<&str, PromptSectionZone> = sections
        .iter()
        .map(|section| (section.id, section.zone))
        .collect();
    let mut rejected = Vec::new();
    let mut accepted_total = 0usize;
    let mut ids: Vec<String> = state.section_overrides.keys().cloned().collect();
    ids.sort();
    for id in ids {
        let Some(text) = state.section_overrides.get(&id).cloned() else {
            continue;
        };
        let Some(zone) = zones.get(id.as_str()).copied() else {
            state.section_overrides.remove(&id);
            rejected.push((id, OverrideRejection::SectionAbsent));
            continue;
        };
        match validate_section_override(zone, &text) {
            Ok(()) => {
                let total = accepted_total + text.len();
                if total > MAX_TOTAL_OVERRIDE_BYTES {
                    state.section_overrides.remove(&id);
                    rejected.push((
                        id,
                        OverrideRejection::TotalBudgetExceeded {
                            bytes: total,
                            limit: MAX_TOTAL_OVERRIDE_BYTES,
                        },
                    ));
                } else {
                    accepted_total = total;
                }
            }
            Err(reason) => {
                state.section_overrides.remove(&id);
                rejected.push((id, reason));
            }
        }
    }
    rejected
}

/// 记录覆盖拒绝诊断（段落 + 类别 + 有界细节；不输出正文）。
pub(crate) fn log_rejections(rejections: &[(String, OverrideRejection)]) {
    for (section, reason) in rejections {
        tracing::warn!(
            section = %section,
            category = reason.category(),
            detail = %reason.detail(),
            "meta_harness 段落覆盖被拒绝：保留内置段（不静默剥离、不自动改写旧快照）"
        );
    }
}

/// 有界占位符样本（诊断用；不输出疑似正文）。
pub(super) fn bounded_token(token: &str) -> String {
    if token.chars().count() <= DIAGNOSTIC_TOKEN_MAX_CHARS {
        return token.to_string();
    }
    let mut bounded: String = token.chars().take(DIAGNOSTIC_TOKEN_MAX_CHARS).collect();
    bounded.push('…');
    bounded
}
