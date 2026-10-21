//! 窗口诊断计数：只记录数字/身份，不记录正文。

/// 诊断计数。只记录数字/身份，不记录正文。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowStats {
    pub evicted_bytes: u64,
    pub evicted_segments: u64,
    pub evicted_entries: u64,
    pub evict_blocked_pinned: u64,
    pub admission_blocked: u64,
    pub stale_pages: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryWindowDiagnostics {
    pub tui_history_body_bytes: usize,
    /// 已归档但尚无持久身份、不可淘汰的实时单元正文（工具卡/子 Agent 等）。
    pub tui_unattributed_live_bytes: usize,
    pub tui_history_reserved_bytes: usize,
    pub tui_pinned_bytes: usize,
    pub history_page_reserved_bytes: usize,
    pub history_page_pending_bytes: usize,
    pub detail_window_bytes: usize,
    pub resident_segments: usize,
    pub gap_count: usize,
    pub evicted_bytes: u64,
    pub evict_blocked_pinned: u64,
    pub admission_blocked: u64,
    pub stale_pages: u64,
    pub window_degraded: bool,
}
