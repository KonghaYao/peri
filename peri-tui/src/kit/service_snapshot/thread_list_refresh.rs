//! 会话列表协调器：维护 scope、归档和分页观察值及其 last-good 列表。
//!
//! 这个协调器只负责会话列表查询与 2 秒节流；cwd、scope、archived 或 page_count
//! 变化会立即触发同页重读。查询始终按已加载页数逐页读取，不共享跨会话缓存，
//! 也不直接发布 atom；提交由统一失效门负责。

use std::time::{Duration, Instant};

use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::{THREAD_LIST_PAGE_SIZE, ThreadBrowserScope, ThreadSummary};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadQuery, ThreadScope};

pub(super) struct ThreadListRefresh {
    pub(super) cwd: String,
    pub(super) workspace: Option<ResolvedWorkspace>,
    pub(super) scope: ThreadBrowserScope,
    pub(super) archived: bool,
    pub(super) page_count: u32,
    pub(super) next: Instant,
    pub(super) entries: Vec<ThreadSummary>,
}

impl Default for ThreadListRefresh {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            workspace: None,
            scope: ThreadBrowserScope::default(),
            archived: false,
            page_count: 1,
            next: peri_time::monotonic_now(),
            entries: Vec::new(),
        }
    }
}

pub(super) struct ThreadListObservation {
    pub(super) entries: Vec<ThreadSummary>,
    pub(super) error: Option<String>,
    pub(super) has_more: bool,
    pub(super) refreshed: bool,
}

impl ThreadListRefresh {
    pub(super) async fn refresh(
        &mut self,
        now: Instant,
        client: Option<&AcpTuiClient>,
        cwd: &str,
        scope: ThreadBrowserScope,
        archived: bool,
        page_count: u32,
    ) -> ThreadListObservation {
        let changed = cwd != self.cwd
            || scope != self.scope
            || archived != self.archived
            || page_count != self.page_count;
        if now >= self.next || changed {
            if cwd != self.cwd {
                self.workspace = None;
            }
            self.cwd = cwd.to_string();
            self.scope = scope;
            self.archived = archived;
            self.page_count = page_count;
            if let Some(client) = client {
                match self.query(client).await {
                    Ok((entries, has_more)) => {
                        self.entries = entries;
                        self.next = now + Duration::from_secs(2);
                        return ThreadListObservation {
                            entries: self.entries.clone(),
                            error: None,
                            has_more,
                            refreshed: true,
                        };
                    }
                    Err(error) => {
                        self.entries.clear();
                        self.next = now + Duration::from_secs(2);
                        return ThreadListObservation {
                            entries: Vec::new(),
                            error: Some(error.to_string()),
                            has_more: false,
                            refreshed: true,
                        };
                    }
                }
            }
            self.next = now + Duration::from_secs(2);
        }
        ThreadListObservation {
            entries: self.entries.clone(),
            error: None,
            has_more: false,
            refreshed: false,
        }
    }

    async fn query(
        &mut self,
        client: &AcpTuiClient,
    ) -> Result<(Vec<ThreadSummary>, bool), peri_acp::transport::types::AcpError> {
        if self.scope != ThreadBrowserScope::All && self.workspace.is_none() {
            self.workspace = Some(
                client
                    .session_context(None, Some(&self.cwd))
                    .await?
                    .workspace,
            );
        }
        let thread_scope = match self.scope {
            ThreadBrowserScope::Project => ThreadScope::Project(
                self.workspace
                    .as_ref()
                    .expect("workspace resolved")
                    .project_id,
            ),
            ThreadBrowserScope::Workspace => ThreadScope::Workspace(
                self.workspace
                    .as_ref()
                    .expect("workspace resolved")
                    .workspace_id,
            ),
            ThreadBrowserScope::All => ThreadScope::All,
        };
        let mut cursor = None;
        let mut entries = Vec::new();
        for _ in 0..self.page_count.max(1) {
            let query = ScopedThreadQuery {
                scope: thread_scope.clone(),
                cursor,
                limit: THREAD_LIST_PAGE_SIZE,
            };
            let page = if self.archived {
                client.list_archived_threads(&query).await?
            } else {
                client.list_scoped_threads(&query).await?
            };
            entries.extend(page.entries.into_iter().map(|entry| ThreadSummary {
                id: entry.thread.id,
                title: entry.thread.title,
                cwd: entry.effective_cwd.to_string_lossy().into_owned(),
                message_count: entry.thread.message_count,
                updated_at: Some(entry.thread.updated_at),
            }));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok((entries, cursor.is_some()))
    }
}
