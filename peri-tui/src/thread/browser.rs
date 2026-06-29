use std::sync::Arc;

use super::{ThreadId, ThreadMeta, ThreadStore};
use crate::app::FieldTextarea;

/// TUI 内 Thread 历史浏览面板
#[derive(Clone)]
pub struct ThreadBrowser {
    /// 全量 thread 列表（按 updated_at 降序）
    pub threads: Vec<ThreadMeta>,
    /// 当前光标位置（指向过滤后列表的索引）
    pub cursor: usize,
    pub store: Arc<dyn ThreadStore>,
    /// 内容滚动偏移
    pub scroll_offset: u16,
    /// 是否处于删除确认状态
    pub confirm_delete: bool,
    /// 搜索输入状态
    pub search_query: FieldTextarea,
    /// 搜索框是否聚焦
    pub search_focused: bool,
    /// 当前 cwd 的 git 分支
    pub branch: Option<String>,
    /// 过滤后的索引映射（存储 threads 中的原始索引）
    filtered_indices: Vec<usize>,
}

impl ThreadBrowser {
    pub fn new(
        threads: Vec<ThreadMeta>,
        store: Arc<dyn ThreadStore>,
        branch: Option<String>,
    ) -> Self {
        let filtered_indices: Vec<usize> = (0..threads.len()).collect();
        Self {
            threads,
            cursor: 0,
            store,
            scroll_offset: 0,
            confirm_delete: false,
            search_query: FieldTextarea::single_line(),
            search_focused: true,
            branch,
            filtered_indices,
        }
    }

    /// 过滤后的 thread 总数
    pub fn total(&self) -> usize {
        self.filtered_indices.len()
    }

    /// 全量 thread 总数
    pub fn total_all(&self) -> usize {
        self.threads.len()
    }

    /// 重新计算过滤索引
    pub fn refresh_filter(&mut self) {
        let query = self.search_query.value().to_lowercase();
        self.filtered_indices = if query.is_empty() {
            (0..self.threads.len()).collect()
        } else {
            self.threads
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    t.title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                })
                .map(|(i, _)| i)
                .collect()
        };
        // 光标修正
        if self.cursor >= self.filtered_indices.len() {
            self.cursor = self.filtered_indices.len().saturating_sub(1);
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let total = self.total();
        if total == 0 {
            return;
        }
        self.cursor = ((self.cursor as isize + delta).rem_euclid(total as isize)) as usize;
    }

    /// 获取光标指向的过滤后 thread
    pub fn selected_thread(&self) -> Option<&ThreadMeta> {
        self.filtered_indices
            .get(self.cursor)
            .and_then(|&idx| self.threads.get(idx))
    }

    /// 获取光标指向的 ThreadId
    pub fn selected_id(&self) -> Option<&ThreadId> {
        self.selected_thread().map(|t| &t.id)
    }

    /// 删除光标所在的历史 thread（同步，block_in_place），返回被删除的对话标题
    pub fn delete_selected(&mut self) -> Option<String> {
        let &orig_idx = self.filtered_indices.get(self.cursor)?;
        let meta = self.threads.get(orig_idx)?;
        let id = meta.id.clone();
        let title = meta.title.clone().unwrap_or_else(|| "(无标题)".to_string());
        let store = self.store.clone();
        let ok = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(store.delete_thread(&id))
                .is_ok()
        });
        if ok {
            self.threads.remove(orig_idx);
            // 重建过滤索引
            self.refresh_filter();
            Some(title)
        } else {
            None
        }
    }

    /// 获取过滤后的 thread 列表引用
    pub fn filtered_threads(&self) -> Vec<&ThreadMeta> {
        self.filtered_indices
            .iter()
            .filter_map(|&idx| self.threads.get(idx))
            .collect()
    }
}
