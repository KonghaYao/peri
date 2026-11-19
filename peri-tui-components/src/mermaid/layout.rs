//! 图几何缓存：共享排版结果，按源码、精确列宽、引擎版本与排版参数区分。
//!
//! 主题只影响着色（`display` 在打印时上色），不参与键；窗口高度只影响展示
//! 模式，也不参与键。相同模板的两张图命中同一份几何，字节只计价一次。

use std::collections::HashMap;
use std::sync::Arc;

/// 上游库版本或排版选项语义变化时递增，使旧几何失效。
pub(crate) const ENGINE_VERSION: u32 = 1;
/// `RenderOptions` 的紧凑参数标识；修改参数时递增。
pub(crate) const LAYOUT_PARAMS: u32 = 2;

/// 单张图源码上限：超过后不排队完整排版，保留源码（概览由第 4 部分承接）。
pub(crate) const MAX_LAYOUT_SOURCE_BYTES: usize = 128 * 1024;
/// 单张图输出上限：排版结果超过后视为过预算，保留源码。
pub(crate) const MAX_LAYOUT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// 全局排队上限；运行中的任务串行执行，最多一个。
pub(crate) const MAX_PENDING_JOBS: usize = 16;
/// 几何硬预算。transcript 的 8 MiB 统计另外只按当前引用计价。
pub(crate) const GEOMETRY_BUDGET: usize = 6 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LayoutKind {
    Preview,
    Final,
}

/// 几何键。`source` 共享 `Arc`，插入与查询不复制整份源码。
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct LayoutKey {
    pub(crate) kind: LayoutKind,
    pub(crate) source: Arc<String>,
    pub(crate) width: usize,
    engine: u32,
    params: u32,
}

impl LayoutKey {
    #[must_use]
    pub(crate) fn new(kind: LayoutKind, source: Arc<String>, width: usize) -> Self {
        Self {
            kind,
            source,
            width,
            engine: ENGINE_VERSION,
            params: LAYOUT_PARAMS,
        }
    }
}

#[derive(Debug)]
struct GeometryEntry {
    lines: Arc<Vec<String>>,
    bytes: usize,
    accessed: u64,
}

/// 进程级几何 LRU。条目按内容键去重；调用方只拿共享 `Arc`。
#[derive(Debug, Default)]
pub(crate) struct GeometryCache {
    entries: HashMap<LayoutKey, GeometryEntry>,
    retained: usize,
    clock: u64,
}

impl GeometryCache {
    #[must_use]
    pub(crate) fn get(&mut self, key: &LayoutKey) -> Option<Arc<Vec<String>>> {
        self.get_with_bytes(key).map(|(lines, _)| lines)
    }

    #[must_use]
    pub(crate) fn get_with_bytes(&mut self, key: &LayoutKey) -> Option<(Arc<Vec<String>>, usize)> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.accessed = self.clock;
        Some((Arc::clone(&entry.lines), entry.bytes))
    }

    /// 插入一份几何；返回是否为新的排版结果（缓存命中返回 `false`），以及为守住
    /// 预算而淘汰的键。调用方须释放仍引用这些键的实例帧，否则帧会继续持有行分配。
    #[must_use]
    pub(crate) fn insert(
        &mut self,
        key: LayoutKey,
        lines: Arc<Vec<String>>,
    ) -> (bool, Vec<LayoutKey>) {
        if self.entries.contains_key(&key) {
            return (false, Vec::new());
        }
        let bytes = geometry_bytes(&lines);
        self.clock = self.clock.wrapping_add(1);
        self.retained = self.retained.saturating_add(bytes);
        self.entries.insert(
            key,
            GeometryEntry {
                lines,
                bytes,
                accessed: self.clock,
            },
        );
        let removed = if self.retained > GEOMETRY_BUDGET {
            self.evict_to(GEOMETRY_BUDGET * 3 / 4)
        } else {
            Vec::new()
        };
        (true, removed)
    }

    #[must_use]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained
    }

    /// 淘汰到预算内；返回被移除的键，调用方可据此释放实例帧。
    pub(crate) fn evict_to(&mut self, budget: usize) -> Vec<LayoutKey> {
        let mut removed = Vec::new();
        while self.retained > budget {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.accessed)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.retained = self.retained.saturating_sub(entry.bytes);
                removed.push(key);
            }
        }
        removed
    }
}

#[must_use]
pub(crate) fn geometry_bytes(lines: &[String]) -> usize {
    std::mem::size_of::<Vec<String>>()
        + std::mem::size_of_val(lines)
        + lines.iter().map(String::capacity).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> Arc<String> {
        Arc::new(text.to_string())
    }

    #[test]
    fn geometry_key_is_exact_on_source_width_engine_and_params() {
        let base = LayoutKey::new(LayoutKind::Final, source("graph TD\nA-->B"), 80);
        assert_eq!(
            base,
            LayoutKey::new(LayoutKind::Final, source("graph TD\nA-->B"), 80)
        );
        assert_ne!(
            base,
            LayoutKey::new(LayoutKind::Final, source("graph TD\nA-->B"), 40)
        );
        assert_ne!(
            base,
            LayoutKey::new(LayoutKind::Final, source("graph TD\nA-->C"), 80)
        );
        assert_ne!(
            base,
            LayoutKey::new(LayoutKind::Preview, source("graph TD\nA-->B"), 80)
        );
    }

    #[test]
    fn identical_layouts_are_stored_once_and_evicted_as_a_unit() {
        let mut cache = GeometryCache::default();
        let key = LayoutKey::new(LayoutKind::Final, source("graph TD\nA-->B"), 80);
        let lines = Arc::new(vec!["┌───┐".to_string(), "└───┘".to_string()]);
        assert!(cache.insert(key.clone(), Arc::clone(&lines)).0);
        assert!(!cache.insert(key.clone(), Arc::clone(&lines)).0);
        assert_eq!(cache.retained_bytes(), geometry_bytes(&lines));
        assert!(cache.get(&key).is_some());
        let removed = cache.evict_to(0);
        assert_eq!(removed, vec![key]);
        assert_eq!(cache.retained_bytes(), 0);
    }

    #[test]
    fn budgeting_during_insert_reports_the_evicted_keys() {
        let mut cache = GeometryCache::default();
        let mut keys = Vec::new();
        for index in 0..5 {
            let key = LayoutKey::new(
                LayoutKind::Final,
                source(&format!("graph TD\nA{index}-->B")),
                80,
            );
            let lines = Arc::new(vec!["x".repeat(512 * 1024); 3]);
            let (inserted, removed) = cache.insert(key.clone(), lines);
            assert!(inserted);
            keys.push(key);
            if !removed.is_empty() {
                assert_eq!(removed[0], keys[0], "oldest entry goes first");
            }
        }
        assert!(cache.get(&keys[0]).is_none());
        assert!(cache.get(&keys[4]).is_some());
        assert!(cache.retained_bytes() <= GEOMETRY_BUDGET);
    }
}
