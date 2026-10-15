//! Memory 资源协调器：按原有 30 秒节奏扫描 `~/.claude/memory`。
//!
//! 这个协调器只负责 memory 列表的 last-good 缓存与阻塞扫描，不参与会话身份
//! 校验，也不直接发布 atom；扫描失败按原语义保留为空列表。

use std::time::{Duration, Instant};

use crate::kit::atoms::MemoryEntry;

pub(super) struct MemoryScan {
    pub(super) next: Instant,
    pub(super) entries: Vec<MemoryEntry>,
}

impl Default for MemoryScan {
    fn default() -> Self {
        Self {
            next: peri_time::monotonic_now(),
            entries: Vec::new(),
        }
    }
}

impl MemoryScan {
    pub(super) async fn refresh(&mut self, now: Instant) {
        if now < self.next {
            return;
        }
        self.entries = match tokio::task::spawn_blocking(scan_memory_dir).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, "service_snapshot: memory scan task failed");
                Vec::new()
            }
        };
        self.next = now + Duration::from_secs(30);
    }
}

/// 扫描 `~/.claude/memory` 下的 Markdown 文件，最多返回 200 项。
pub(super) fn scan_memory_dir() -> Vec<MemoryEntry> {
    use std::fs;
    use std::path::PathBuf;

    let Some(home) = dirs_next::home_dir() else {
        return Vec::new();
    };
    let memory_dir: PathBuf = home.join(".claude").join("memory");
    if !memory_dir.exists() || !memory_dir.is_dir() {
        return Vec::new();
    }
    let entries = match fs::read_dir(&memory_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let relative = path
            .strip_prefix(&memory_dir)
            .ok()
            .map(|value| value.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if relative.is_empty() {
            continue;
        }
        let size_bytes = entry.metadata().map(|value| value.len()).unwrap_or(0);
        let modified = entry
            .metadata()
            .and_then(|value| value.modified())
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from);
        result.push(MemoryEntry {
            path: relative,
            size_bytes,
            modified,
        });
        if result.len() >= 200 {
            break;
        }
    }
    result
}
