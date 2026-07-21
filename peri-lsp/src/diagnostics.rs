use std::collections::{HashMap, HashSet};

use lsp_types::DiagnosticSeverity as LspDiagnosticSeverity;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::protocol::lsp_types::PublishDiagnosticsParams;

type DiagnosticCallback = Box<dyn Fn(Vec<DiagnosticEntry>) + Send + Sync>;

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
const LRU_CACHE_CAPACITY: usize = 500;

/// 诊断注册表（被动推送 + 去重 + 限流）
pub struct DiagnosticsRegistry {
    /// 当前活跃诊断（按文件 URI 索引）
    current: RwLock<HashMap<String, Vec<DiagnosticEntry>>>,
    /// 跨轮次已推送诊断的 key（用于去重）
    delivered: Mutex<lru::LruCache<String, HashSet<String>>>,
    /// 事件回调
    on_update: RwLock<Option<DiagnosticCallback>>,
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
            delivered: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(LRU_CACHE_CAPACITY).unwrap(),
            )),
            on_update: RwLock::new(None),
        }
    }

    /// 注册更新回调
    pub fn on_update(&self, callback: DiagnosticCallback) {
        *self.on_update.write() = Some(callback);
    }

    /// 处理 textDocument/publishDiagnostics 通知
    pub fn handle_publish_diagnostics(&self, params: &PublishDiagnosticsParams) {
        let uri = params.uri.to_string();

        // 诊断为空数组表示清除
        if params.diagnostics.is_empty() {
            self.current.write().remove(&uri);
            self.delivered.lock().pop(&uri);
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

        // 去重：过滤掉已推送的诊断
        let mut new_entries = Vec::new();
        {
            let mut delivered = self.delivered.lock();
            let file_delivered = delivered.get_or_insert_mut(uri.clone(), HashSet::new);

            for entry in entries.into_iter() {
                let key = format!(
                    "{:?}:{}:{}:{}",
                    entry.severity, entry.line, entry.character, entry.message
                );
                if !file_delivered.contains(&key) {
                    file_delivered.insert(key);
                    new_entries.push(entry);
                }
            }
        }

        if new_entries.is_empty() {
            return;
        }

        // 更新当前诊断
        {
            let mut current = self.current.write();
            current.insert(uri.clone(), new_entries.clone());
        }

        // 总量限流
        let all_entries = {
            let current = self.current.read();
            let mut all: Vec<DiagnosticEntry> = current.values().flatten().cloned().collect();
            all.sort_by_key(|e| e.severity);
            all.truncate(MAX_TOTAL_DIAGNOSTICS);
            all
        };

        // 触发回调
        if let Some(ref callback) = *self.on_update.read() {
            callback(new_entries);
        }

        let _ = all_entries; // 可用于后续主动查询
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
        self.delivered.lock().clear();
    }
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
