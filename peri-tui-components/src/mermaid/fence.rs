//! Mermaid 围栏元数据与图身份。
//!
//! Markdown 解析层负责扫描原文并构造 [`MermaidFence`]；组件只读取正文、闭合
//! 状态与原文坐标，不依赖任何 Markdown 解析器。

/// 围栏标记种类（CommonMark 只允许反引号或波浪号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FenceKind {
    Backtick,
    Tilde,
}

/// 关闭行是否已经被当前缓冲区确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FenceClose {
    /// 关闭行已完整到达（有换行，或调用方声明这是消息 EOF）。
    Closed,
    /// 关闭行当前位于缓冲区末尾且没有换行：下一个 token 仍可能延长它。
    Provisional,
    /// 没有关闭行，正文持续到缓冲区末尾。
    Open,
}

impl FenceClose {
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// 信息串的首个语言标记是否为 `mermaid`（大小写不敏感）。
#[must_use]
pub fn is_mermaid_info(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|tag| tag.eq_ignore_ascii_case("mermaid"))
}

/// 一个 Mermaid 围栏在**原始消息文本**中的位置、闭合状态与正文。
///
/// `raw` 保留原文逐字字节（含引用/列表等容器前缀），供复制使用；
/// `source` 是去掉容器前缀后的 CommonMark 代码正文，供 Mermaid 解析使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidFence {
    pub kind: FenceKind,
    pub marker_len: usize,
    /// 信息串首个语言标记（`mermaid`，大小写保留原样）。
    pub language: String,
    /// 完整信息串。
    pub info: String,
    /// 开围栏标记在原文中的字节位置（容器前缀之后）。
    pub open_start: usize,
    /// 正文起点（开围栏行换行之后）。
    pub body_start: usize,
    /// 正文终点（真实关闭行起点或缓冲区末尾）。
    pub body_end: usize,
    /// 整个围栏块终点（关闭行换行之后）。
    pub end: usize,
    pub close: FenceClose,
    /// 开围栏行是否已由换行（或消息 EOF）确认，信息串不会继续增长。
    pub opener_confirmed: bool,
    /// 原始开围栏行（含容器前缀），用于正文整体改写后的位置复核。
    pub opening_line: String,
    /// 原文正文逐字字节（含容器前缀）。
    pub raw: String,
    /// 去掉容器前缀后的正文（CommonMark Text 事件内容）。
    pub source: String,
}

impl MermaidFence {
    #[must_use]
    pub fn closed(&self) -> bool {
        self.close.is_closed()
    }

    /// 正文整体改写后，是否仍可把新围栏视为同一个实例。
    ///
    /// 只核对位置与开围栏行：正文在流式追加中本来就会增长，不能参与身份判定。
    #[must_use]
    pub fn reuses_position(&self, previous: &Self) -> bool {
        self.open_start == previous.open_start
            && self.kind == previous.kind
            && self.marker_len == previous.marker_len
            && self.language.eq_ignore_ascii_case(&previous.language)
            && self.opening_line == previous.opening_line
    }

    /// 组合会话、消息身份与围栏位置，得到一处图的稳定身份。
    #[must_use]
    pub fn key(
        &self,
        session_id: impl Into<String>,
        message: MermaidMessageRef,
    ) -> MermaidFenceKey {
        MermaidFenceKey {
            message: MermaidMessageIdentity {
                session_id: session_id.into(),
                message,
            },
            open_start: self.open_start,
        }
    }

    /// 分块解析时把片段内坐标转换为完整消息坐标。
    pub fn shift_origin(&mut self, base: usize) {
        if base == 0 {
            return;
        }
        self.open_start = self.open_start.saturating_add(base);
        self.body_start = self.body_start.saturating_add(base);
        self.body_end = self.body_end.saturating_add(base);
        self.end = self.end.saturating_add(base);
    }
}

/// 消息稳定身份的两种来源。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MermaidMessageRef {
    /// ACP `messageId` 已到达。
    Persistent(String),
    /// 持久 ID 未到达时，随气泡存续的本地身份。
    Local(u64),
}

impl MermaidMessageRef {
    /// 从气泡字段推导消息身份。
    ///
    /// `started_at` 在 Clone、折叠写回和 `Arc::make_mut` 替换后保持不变，
    /// 适合作为流式气泡的本地句柄；冻结且没有持久 ID 的极端路径退回内容哈希。
    #[must_use]
    pub fn for_bubble(
        message_id: Option<&str>,
        started_at: Option<std::time::Instant>,
        stable_content_hash: u64,
    ) -> Self {
        if let Some(message_id) = message_id {
            return Self::Persistent(message_id.to_string());
        }
        let Some(started_at) = started_at else {
            return Self::Local(stable_content_hash);
        };
        use std::hash::Hash;
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        started_at.hash(&mut hasher);
        Self::Local(hasher.finish())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MermaidMessageIdentity {
    pub session_id: String,
    pub message: MermaidMessageRef,
}

/// 一处 Mermaid 图的稳定身份：会话 + 消息 + 围栏在原文中的位置。
///
/// [Why 只用 open_start] 正文与关闭位置在流式追加中会变化；围栏起点在消息
/// 原文中唯一且稳定。相同源码出现两次时，两个围栏的起点不同，状态各自独立。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MermaidFenceKey {
    pub message: MermaidMessageIdentity,
    pub open_start: usize,
}

#[cfg(test)]
#[path = "fence_test.rs"]
mod tests;
