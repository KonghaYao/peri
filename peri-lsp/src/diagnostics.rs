use std::collections::HashMap;

use lsp_types::DiagnosticSeverity as LspDiagnosticSeverity;
use parking_lot::RwLock;
use serde::Serialize;

use crate::protocol::lsp_types::PublishDiagnosticsParams;

/// 单条诊断的精简表示
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticEntry {
    pub file_uri: String,
    pub line: u32,
    pub character: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl From<LspDiagnosticSeverity> for DiagnosticSeverity {
    fn from(s: LspDiagnosticSeverity) -> Self {
        match s {
            LspDiagnosticSeverity::ERROR => DiagnosticSeverity::Error,
            LspDiagnosticSeverity::WARNING => DiagnosticSeverity::Warning,
            LspDiagnosticSeverity::INFORMATION => DiagnosticSeverity::Information,
            LspDiagnosticSeverity::HINT => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Information,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiagnosticSummary {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
    pub hints: usize,
    pub files_with_errors: usize,
}

impl DiagnosticSummary {
    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.info + self.hints
    }
}

const MAX_DIAGNOSTICS_PER_FILE: usize = 10;
const MAX_TOTAL_DIAGNOSTICS: usize = 30;

/// 诊断注册表（被动推送 + 限流）
///
/// 消费方通过 `get_for_file`/`get_all`/`summary` 主动查询；
/// 历史遗留的 on_update 推送回调（含其 delivered 去重缓存）无任何生产消费者，
/// 仅测试注册过，已于清理时移除。
pub struct DiagnosticsRegistry {
    /// 当前活跃诊断（按文件 URI 索引）
    current: RwLock<HashMap<String, Vec<DiagnosticEntry>>>,
}

impl Default for DiagnosticsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsRegistry {
    pub fn new() -> Self {
        Self {
            current: RwLock::new(HashMap::new()),
        }
    }

    /// 处理 textDocument/publishDiagnostics 通知
    pub fn handle_publish_diagnostics(&self, params: &PublishDiagnosticsParams) {
        let uri = params.uri.to_string();

        // 诊断为空数组表示清除
        if params.diagnostics.is_empty() {
            self.current.write().remove(&uri);
            return;
        }

        // 转换并排序
        let mut entries: Vec<DiagnosticEntry> = params
            .diagnostics
            .iter()
            .map(|d| DiagnosticEntry {
                file_uri: uri.clone(),
                line: d.range.start.line + 1, // 0-based -> 1-based
                character: d.range.start.character + 1, // 0-based -> 1-based
                severity: d
                    .severity
                    .unwrap_or(LspDiagnosticSeverity::INFORMATION)
                    .into(),
                message: d.message.clone(),
                source: d.source.clone(),
            })
            .collect();

        // 按严重程度排序
        entries.sort_by_key(|e| e.severity);

        // 每文件限流
        entries.truncate(MAX_DIAGNOSTICS_PER_FILE);

        // 以服务器发布的完整集合为准写入（覆盖旧集合，含与上次相同的条目）
        self.current.write().insert(uri, entries);
    }

    /// 主动查询指定文件的诊断
    pub fn get_for_file(&self, uri: &str) -> Vec<DiagnosticEntry> {
        self.current.read().get(uri).cloned().unwrap_or_default()
    }

    /// 获取所有活跃诊断
    pub fn get_all(&self) -> Vec<DiagnosticEntry> {
        let current = self.current.read();
        let mut all: Vec<DiagnosticEntry> = current.values().flatten().cloned().collect();
        all.sort_by_key(|e| e.severity);
        all.truncate(MAX_TOTAL_DIAGNOSTICS);
        all
    }

    /// 获取诊断统计
    pub fn summary(&self) -> DiagnosticSummary {
        let current = self.current.read();
        let mut summary = DiagnosticSummary::default();
        for entries in current.values() {
            let has_error = entries
                .iter()
                .any(|e| e.severity == DiagnosticSeverity::Error);
            if has_error {
                summary.files_with_errors += 1;
            }
            for e in entries {
                match e.severity {
                    DiagnosticSeverity::Error => summary.errors += 1,
                    DiagnosticSeverity::Warning => summary.warnings += 1,
                    DiagnosticSeverity::Information => summary.info += 1,
                    DiagnosticSeverity::Hint => summary.hints += 1,
                }
            }
        }
        summary
    }

    /// 清除所有诊断
    pub fn clear_all(&self) {
        self.current.write().clear();
    }
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
