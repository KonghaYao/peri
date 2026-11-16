//! Canonical Discover state: query, request ownership, results, and selected identity.

use super::{DiscoverDetailAction, PluginSearchResultItem, SearchState};
use crate::application::operations::{CompletionGate, OperationScope, OperationTicket};
use crate::components::textarea::TextAreaState;

pub(super) struct DiscoverState {
    pub editor: TextAreaState,
    pub focused: bool,
    pub status: SearchState,
    pub selected: usize,
    pub detail: Option<PluginSearchResultItem>,
    pub detail_action: usize,
    // 远程搜索的在途身份：作用域、代次与取消由原语维护，晚到结果在这里被拒收。
    gate: CompletionGate,
    // None is local browsing; Some(empty) is an authoritative empty search result.
    results: Option<Vec<PluginSearchResultItem>>,
}

impl Default for DiscoverState {
    fn default() -> Self {
        Self {
            editor: TextAreaState::default(),
            focused: false,
            status: SearchState::Idle,
            selected: 0,
            detail: None,
            detail_action: 0,
            gate: CompletionGate::default(),
            results: None,
        }
    }
}

impl DiscoverState {
    /// 编辑器、结果与在途搜索一起失效：改字、清空、关闭都经这里，
    /// 被替代的响应此后不会落地。
    fn invalidate(&mut self) {
        self.gate.cancel();
        self.status = SearchState::Idle;
        self.results = None;
        self.selected = 0;
        self.close_detail();
    }

    pub fn reset_session(&mut self, scope: OperationScope) {
        // 只认作用域变化，不认「又投影了一次」：同作用域重复调用不得打断在途搜索。
        if self.gate.reset_scope(scope) {
            self.close();
        }
    }

    pub fn close(&mut self) {
        self.invalidate();
        self.editor = TextAreaState::default();
        self.focused = false;
    }

    pub fn edit(&mut self, action: impl FnOnce(&mut TextAreaState)) {
        self.invalidate();
        action(&mut self.editor);
        self.focused = true;
    }

    pub fn begin_search(&mut self, scope: OperationScope) -> Option<OperationTicket> {
        if self.editor.text.is_empty() {
            return None;
        }
        // Session changes are normally projected before user input; reject a
        // stale editor instead of submitting it under a new session.
        if !self.gate.scope_matches(&scope) {
            self.reset_session(scope);
            return None;
        }
        self.invalidate();
        self.status = SearchState::Loading;
        self.focused = true;
        Some(self.gate.begin(scope))
    }

    pub fn complete(
        &mut self,
        ticket: &OperationTicket,
        result: Result<Vec<PluginSearchResultItem>, String>,
    ) -> bool {
        // 过期拒收：被替代或跨会话的响应在这里被丢弃，不写入结果与状态。
        if !self.gate.accept(ticket) {
            return false;
        }
        match result {
            Ok(items) => {
                self.results = Some(items);
                self.status = SearchState::Idle;
            }
            Err(error) => {
                self.results = Some(vec![]);
                self.status = SearchState::Error(error);
            }
        }
        self.selected = 0;
        true
    }

    pub fn visible_items<'a>(
        &'a self,
        local: &'a [PluginSearchResultItem],
    ) -> Vec<&'a PluginSearchResultItem> {
        if let Some(results) = &self.results {
            return results.iter().collect();
        }
        let query = self.editor.text.to_lowercase();
        local
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.name.to_lowercase().contains(&query)
                    || item.description.to_lowercase().contains(&query)
                    || item.marketplace.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn open_selected(&mut self, index: usize, local: &[PluginSearchResultItem]) {
        if !matches!(self.status, SearchState::Idle) {
            return;
        }
        let item = self
            .visible_items(local)
            .get(index)
            .map(|item| (*item).clone());
        if let Some(item) = item {
            self.selected = index;
            self.detail = Some(item);
            self.detail_action = 0;
            self.focused = false;
        }
    }

    pub fn close_detail(&mut self) {
        self.detail = None;
        self.detail_action = 0;
    }

    pub fn detail_selection(&self) -> Option<(PluginSearchResultItem, DiscoverDetailAction)> {
        Some((
            self.detail.clone()?,
            *DiscoverDetailAction::ALL.get(self.detail_action)?,
        ))
    }
}
