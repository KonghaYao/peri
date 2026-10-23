//! History V2 页面与条目结构（设计 §8）。
//!
//! 页面是逻辑顺序上的一页；coverage 描述已消费半开区间，而不是由 items 首尾推导。
//! 条目身份由存储/投影稳定产生；客户端只做类型层校验，不推断时间戳或连续行号。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::body::HistoryBodyDescriptor;
use super::error::HistoryContractError;
use super::integer::DecimalU64;
use super::negotiation::HistoryLimits;
use super::request::HistorySource;
use super::{deserialize_required_option, require_identity};

/// `HistoryWindowPage.kind`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryWindowKind {
    Window,
}

/// 一页历史窗口；open/page/seek 共用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryWindowPage {
    pub version: u32,
    pub kind: HistoryWindowKind,
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub history_revision: DecimalU64,
    /// 必填，可 null；null 表示空快照的有效上界。
    #[serde(deserialize_with = "deserialize_required_option")]
    pub committed_tail: Option<DecimalU64>,
    pub projection_version: u32,
    pub read_ticket: String,
    /// 必填，可 null；null 表示该方向的快照逻辑范围已耗尽。
    #[serde(deserialize_with = "deserialize_required_option")]
    pub before_cursor: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub after_cursor: Option<String>,
    pub items: Vec<HistoryEntry>,
    pub coverage: HistoryCoverage,
    pub progress: HistoryProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// 展示目录条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryEntry {
    pub entry_id: String,
    pub source: HistorySource,
    /// 页面条目的版本占位值。
    ///
    /// 契约：页面阶段不读正文，这里恒为 `"1"`，**不用于冲突检测或内容一致性判断**。
    /// 需要稳定内容身份时使用 body chunk 的 `contentVersion`（canonical payload hash）。
    pub content_version: String,
    pub durability: HistoryDurability,
    pub order: HistoryOrder,
    pub sub_items: Vec<HistorySubItem>,
}

/// 持久封口事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryDurability {
    pub state: HistoryDurabilityState,
    pub origin: HistoryOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_item_seq: Option<DecimalU64>,
}

impl HistoryDurability {
    /// live 必须携带 `finalItemSeq`；imported 必须没有。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        match self.origin {
            HistoryOrigin::Live if self.final_item_seq.is_none() => Err(
                HistoryContractError::shape("live durability requires finalItemSeq"),
            ),
            HistoryOrigin::Imported if self.final_item_seq.is_some() => Err(
                HistoryContractError::shape("imported durability must not carry finalItemSeq"),
            ),
            HistoryOrigin::Live | HistoryOrigin::Imported => Ok(()),
        }
    }
}

/// 封口状态；V2 只发布已封口条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryDurabilityState {
    Sealed,
}

/// 条目来源代次。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryOrigin {
    Live,
    Imported,
}

/// 同一 snapshot/source 布局内的排序键。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryOrder {
    /// 布局 region 的不透明字符串身份（不是数字）。
    pub region: String,
    pub ordinal: DecimalU64,
}

/// 已批准展示的条目子项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistorySubItem {
    Text(HistoryTextSubItem),
    Reasoning(HistoryTextSubItem),
    ToolCall(HistoryToolCallSubItem),
    ToolResult(HistoryToolResultSubItem),
    Reminder(HistoryNoticeSubItem),
    Summary(HistoryNoticeSubItem),
    Attachment(HistoryAttachmentSubItem),
    /// 条目已被有界定位，但 payload 未解码（oversize）。
    ///
    /// 客户端先渲染占位，再按 `body` descriptor 分块取正文；该变体不携带任何
    /// 内联正文，因此 descriptor 本身必须有界。
    Opaque(HistoryOpaqueSubItem),
}

impl HistorySubItem {
    /// 按协商预算校验 identity、内联内容与正文描述符。
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        match self {
            Self::Text(item) | Self::Reasoning(item) => item.validate_bounded(limits),
            Self::ToolCall(item) => item.validate_bounded(limits),
            Self::ToolResult(item) => item.validate_bounded(limits),
            Self::Reminder(item) | Self::Summary(item) => item.validate_bounded(limits),
            Self::Attachment(item) => item.validate_bounded(limits),
            Self::Opaque(item) => item.validate_bounded(limits),
        }
    }
}

/// text / reasoning 共用形状。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryTextSubItem {
    pub sub_item_id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HistoryBodyDescriptor>,
}

impl HistoryTextSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        require_exactly_one("text/reasoning", self.text.is_some(), self.body.is_some())?;
        if let Some(text) = &self.text {
            require_inline_bytes("text", text.len(), limits)?;
        }
        if let Some(body) = &self.body {
            body.validate(limits)?;
        }
        Ok(())
    }
}

/// toolCall 子项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryToolCallSubItem {
    pub sub_item_id: String,
    pub invocation_key: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HistoryBodyDescriptor>,
}

impl HistoryToolCallSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        require_identity("invocationKey", &self.invocation_key)?;
        require_exactly_one("toolCall input", self.input.is_some(), self.body.is_some())?;
        if let Some(input) = &self.input {
            let bytes = serde_json::to_vec(input)
                .map_err(|_| HistoryContractError::shape("toolCall input could not be measured"))?;
            require_inline_bytes("input", bytes.len(), limits)?;
        }
        if let Some(body) = &self.body {
            body.validate(limits)?;
        }
        Ok(())
    }
}

/// toolResult 子项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryToolResultSubItem {
    pub sub_item_id: String,
    pub invocation_key: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HistoryBodyDescriptor>,
}

impl HistoryToolResultSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        require_identity("invocationKey", &self.invocation_key)?;
        require_exactly_one(
            "toolResult output",
            self.output.is_some(),
            self.body.is_some(),
        )?;
        if let Some(output) = &self.output {
            require_inline_bytes("output", output.len(), limits)?;
        }
        if let Some(body) = &self.body {
            body.validate(limits)?;
        }
        Ok(())
    }
}

/// reminder / summary 共用形状。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryNoticeSubItem {
    pub sub_item_id: String,
    pub category: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HistoryBodyDescriptor>,
}

impl HistoryNoticeSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        require_exactly_one("reminder/summary", self.text.is_some(), self.body.is_some())?;
        if let Some(text) = &self.text {
            require_inline_bytes("text", text.len(), limits)?;
        }
        if let Some(body) = &self.body {
            body.validate(limits)?;
        }
        Ok(())
    }
}

/// attachment 子项：正文必须外置，不内联媒体。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryAttachmentSubItem {
    pub sub_item_id: String,
    pub media_type: String,
    pub label: String,
    pub body: HistoryBodyDescriptor,
}

impl HistoryAttachmentSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        self.body.validate(limits)
    }
}

/// oversize 条目的占位子项：只带身份与正文描述符，不承载任何内联正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryOpaqueSubItem {
    pub sub_item_id: String,
    pub body: HistoryBodyDescriptor,
}

impl HistoryOpaqueSubItem {
    pub fn validate_bounded(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_identity("subItemId", &self.sub_item_id)?;
        self.body.validate(limits)
    }
}

fn require_exactly_one(
    label: &'static str,
    inline: bool,
    body: bool,
) -> Result<(), HistoryContractError> {
    if inline == body {
        return Err(HistoryContractError::shape(format!(
            "{label} requires exactly one of inline content or body"
        )));
    }
    Ok(())
}

fn require_inline_bytes(
    field: &'static str,
    len: usize,
    limits: &HistoryLimits,
) -> Result<(), HistoryContractError> {
    if u64::try_from(len).unwrap_or(u64::MAX) > limits.max_response_bytes {
        return Err(HistoryContractError::limits(format!(
            "{field} exceeds maxResponseBytes"
        )));
    }
    Ok(())
}

/// 已消费的逻辑范围。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistoryCoverage {
    EmptySnapshot,
    Interval(HistoryCoverageInterval),
}

/// 半开区间 `[from, to)`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryCoverageInterval {
    pub from: HistoryBoundary,
    pub to: HistoryBoundary,
}

/// 逻辑顺序边界。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistoryBoundary {
    Head,
    Tail,
    Position(HistoryBoundaryPosition),
}

/// region/ordinal/side 定位的位置边界。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryBoundaryPosition {
    pub region: String,
    pub ordinal: DecimalU64,
    pub side: HistoryBoundarySide,
}

/// 位置边界的一侧；同一位置 `before < after`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryBoundarySide {
    Before,
    After,
}

/// 本次读计划的工作量与推进量。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryProgress {
    pub reason: HistoryProgressReason,
    pub scanned_entries: u64,
    pub consumed_entries: u64,
}

impl HistoryProgress {
    /// 扫描预算耗尽必须伴随实际推进（`consumedEntries > 0`）。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if self.reason == HistoryProgressReason::ScanBudgetExhausted && self.consumed_entries == 0 {
            return Err(HistoryContractError::shape(
                "scan_budget_exhausted requires consumedEntries > 0",
            ));
        }
        if self.consumed_entries > self.scanned_entries {
            return Err(HistoryContractError::shape(
                "consumedEntries must not exceed scannedEntries",
            ));
        }
        Ok(())
    }
}

/// 读计划结束原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryProgressReason {
    Complete,
    ScanBudgetExhausted,
}
