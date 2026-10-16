//! 快照提交协调器：集中执行身份失效门并发布类型明确的部分更新。
//!
//! 这个协调器负责两项不变量：一次发布先重验整批观察身份，任何会话或列表视图
//! 变化都拒绝整批结果；通过后只在一个短写锁中修改各补丁声明的
//! `SERVICE_SNAPSHOT` 字段，禁止 clone 后整体替换。各字段仍用 `PartialEq` 短路，
//! 相同值不会产生 atom 更新通知。

use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::ServiceSnapshot;
use crate::kit::atoms::{
    ACTIVE_EXECUTION_CWD, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, CRON_JOBS,
    CURRENT_SESSION_TITLE, FILE_LIST, HOOK_LIST, Handle, MCP_SERVERS, MEMORY_LIST,
    McpServerSummary, MemoryEntry, PLUGIN_LIST, PROVIDER_LIST, PluginSummary, ProviderSummary,
    SERVICE_PROJECTION_ERROR, SERVICE_SNAPSHOT, THREAD_BROWSER_ARCHIVED, THREAD_BROWSER_SCOPE,
    THREAD_LIST, THREAD_LIST_ERROR, THREAD_LIST_HAS_MORE, THREAD_LIST_PAGE_COUNT,
    ThreadBrowserScope, ThreadSummary,
};
use crate::kit::atoms::{CronJobSummary, HookSummary, McpStatusSnapshot};

/// 一批查询开始时捕获的会话与列表视图身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectionIdentity {
    pub(super) active_cwd: Option<String>,
    pub(super) session_id: String,
    pub(super) session_generation: Option<(String, u64)>,
    pub(super) scope: ThreadBrowserScope,
    pub(super) archived: bool,
    pub(super) page_count: u32,
    /// 前端重置代次。会话边界的清场与回退分支把会话号置空（`None` 目标），此时
    /// `session_id` 前后同为空串、`session_generation` 前后同为 `None`——单看上面六项
    /// 分不出批次落在边界哪一侧。`BRIDGE_RESET_COUNTER` 是边界自己的代次，纳入后跨
    /// 边界的批次一律整批放弃；列表面板的回写身份也用同一代次。
    pub(super) bridge_reset: u64,
}

/// 进程采样字段，只能更新 `memory_mb` 与 `cpu_percent`。
#[derive(Clone, Copy, Debug)]
pub(super) struct ProcessSnapshotPatch {
    pub(super) memory_mb: u64,
    pub(super) cpu_percent: f32,
}

/// 会话运行字段，只能更新 provider/model/effort/permission。
#[derive(Clone, Debug)]
pub(super) struct SessionSnapshotPatch {
    pub(super) provider_name: String,
    pub(super) model_alias: String,
    pub(super) model_name: String,
    pub(super) effort: String,
    pub(super) permission_mode: String,
}

/// 会话服务统计字段，只能更新 MCP 与 Cron 汇总。
#[derive(Clone, Debug)]
pub(super) struct ServiceSnapshotPatch {
    pub(super) mcp: McpStatusSnapshot,
    pub(super) cron_total: usize,
    pub(super) cron_enabled: usize,
}

/// 统一入口接受的部分更新。各变体的字段边界就是对应协调器的写权限。
#[derive(Clone, Debug)]
pub(super) enum SnapshotPatch {
    Cwd(String),
    Process(ProcessSnapshotPatch),
    Session(SessionSnapshotPatch),
    Services(ServiceSnapshotPatch),
}

/// 会话运行字段的**部分**更新：`None` 表示本次不改该字段。
///
/// 这是面板侧「已落盘的运行态变更」用的窄入口：只改自己确实改动的字段，不必
/// clone 整份快照再回写（方案 §7.2）。`permission_mode` 不在写权限内——权限模式
/// 由会话投影按观察值发布，面板不直接改。
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionFieldUpdate {
    pub(crate) provider_name: Option<String>,
    pub(crate) model_alias: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) effort: Option<String>,
}

impl SessionFieldUpdate {
    /// 是否有字段需要改写；无改动时连写句柄都不获取。
    fn changes(&self, snapshot: &ServiceSnapshot) -> bool {
        self.provider_name
            .as_ref()
            .is_some_and(|value| snapshot.provider_name != *value)
            || self
                .model_alias
                .as_ref()
                .is_some_and(|value| snapshot.model_alias != *value)
            || self
                .model_name
                .as_ref()
                .is_some_and(|value| snapshot.model_name != *value)
            || self
                .effort
                .as_ref()
                .is_some_and(|value| snapshot.effort != *value)
    }
}

/// 只修改会话运行字段的部分发布入口。
///
/// 与 [`commit_projection`] 共用同一写点与字段边界：在单个短写锁内逐字段比较后
/// 落值，相同值不产生 atom 更新通知。二者差别只在来源——`commit_projection` 提交
/// 一轮查询观察到的完整运行态（需要身份失效门），本函数提交已确认的运行态变更。
pub(crate) fn publish_session_fields(update: SessionFieldUpdate) -> bool {
    let handle = SERVICE_SNAPSHOT.state();
    let changed = {
        let snapshot = handle.read();
        update.changes(&snapshot)
    };
    if !changed {
        return false;
    }
    let mut snapshot = handle.write();
    apply_session_fields(&mut snapshot, &update);
    true
}

/// 逐字段比较后落值：只改 `Some` 的字段，`None` 保持快照现值。
fn apply_session_fields(snapshot: &mut ServiceSnapshot, update: &SessionFieldUpdate) {
    if let Some(provider_name) = &update.provider_name
        && snapshot.provider_name != *provider_name
    {
        snapshot.provider_name = provider_name.clone();
    }
    if let Some(model_alias) = &update.model_alias
        && snapshot.model_alias != *model_alias
    {
        snapshot.model_alias = model_alias.clone();
    }
    if let Some(model_name) = &update.model_name
        && snapshot.model_name != *model_name
    {
        snapshot.model_name = model_name.clone();
    }
    if let Some(effort) = &update.effort
        && snapshot.effort != *effort
    {
        snapshot.effort = effort.clone();
    }
}

impl SnapshotPatch {
    fn matches(&self, snapshot: &ServiceSnapshot) -> bool {
        match self {
            Self::Cwd(cwd) => snapshot.cwd == *cwd,
            Self::Process(process) => {
                snapshot.memory_mb == process.memory_mb
                    && snapshot.cpu_percent == process.cpu_percent.round()
            }
            Self::Session(session) => {
                snapshot.provider_name == session.provider_name
                    && snapshot.model_alias == session.model_alias
                    && snapshot.model_name == session.model_name
                    && snapshot.effort == session.effort
                    && snapshot.permission_mode == session.permission_mode
            }
            Self::Services(services) => {
                snapshot.mcp == services.mcp
                    && snapshot.cron_total == services.cron_total
                    && snapshot.cron_enabled == services.cron_enabled
            }
        }
    }
}

/// 非快照 atoms 的同批发布值，保持原有错误隔离和 last-good 语义。
#[derive(Default)]
pub(super) struct ProjectionAtoms {
    pub(super) title: String,
    pub(super) projection_error: Option<String>,
    pub(super) thread_error: Option<String>,
    pub(super) has_more: bool,
    pub(super) threads: Vec<ThreadSummary>,
    pub(super) cron_jobs: Vec<CronJobSummary>,
    pub(super) files: Vec<String>,
    pub(super) hooks: Vec<HookSummary>,
    pub(super) plugins: Vec<PluginSummary>,
    pub(super) providers: Vec<ProviderSummary>,
    pub(super) mcp_servers: Vec<McpServerSummary>,
    pub(super) memory_entries: Vec<MemoryEntry>,
}

pub(super) fn commit_projection(
    src_client: Option<&AcpTuiClient>,
    expected: &ProjectionIdentity,
    patches: &[SnapshotPatch],
    atoms: ProjectionAtoms,
) -> bool {
    if !identity_is_current(src_client, expected) {
        return false;
    }

    let snapshot_handle = SERVICE_SNAPSHOT.state();
    // 相同值连写句柄也不获取，保留原先 PartialEq 短路及无通知语义。
    let changed = {
        let snapshot = snapshot_handle.read();
        patches.iter().any(|patch| !patch.matches(&snapshot))
    };
    if changed {
        let mut snapshot = snapshot_handle.write();
        for patch in patches {
            match patch {
                SnapshotPatch::Cwd(cwd) => {
                    if snapshot.cwd != *cwd {
                        snapshot.cwd = cwd.clone();
                    }
                }
                SnapshotPatch::Process(process) => {
                    let cpu_percent = process.cpu_percent.round();
                    if snapshot.memory_mb != process.memory_mb {
                        snapshot.memory_mb = process.memory_mb;
                    }
                    if snapshot.cpu_percent != cpu_percent {
                        snapshot.cpu_percent = cpu_percent;
                    }
                }
                SnapshotPatch::Session(session) => {
                    apply_session_fields(
                        &mut snapshot,
                        &SessionFieldUpdate {
                            provider_name: Some(session.provider_name.clone()),
                            model_alias: Some(session.model_alias.clone()),
                            model_name: Some(session.model_name.clone()),
                            effort: Some(session.effort.clone()),
                        },
                    );
                    if snapshot.permission_mode != session.permission_mode {
                        snapshot.permission_mode = session.permission_mode.clone();
                    }
                }
                SnapshotPatch::Services(services) => {
                    if snapshot.mcp != services.mcp {
                        snapshot.mcp = services.mcp.clone();
                    }
                    if snapshot.cron_total != services.cron_total {
                        snapshot.cron_total = services.cron_total;
                    }
                    if snapshot.cron_enabled != services.cron_enabled {
                        snapshot.cron_enabled = services.cron_enabled;
                    }
                }
            }
        }
    }

    write_if_changed(&CURRENT_SESSION_TITLE.state(), atoms.title);
    write_if_changed(&SERVICE_PROJECTION_ERROR.state(), atoms.projection_error);
    write_if_changed(&THREAD_LIST_ERROR.state(), atoms.thread_error);
    write_if_changed(&THREAD_LIST_HAS_MORE.state(), atoms.has_more);
    write_if_changed(&THREAD_LIST.state(), atoms.threads);
    write_if_changed(&CRON_JOBS.state(), atoms.cron_jobs);
    write_if_changed(&FILE_LIST.state(), atoms.files);
    write_if_changed(&HOOK_LIST.state(), atoms.hooks);
    write_if_changed(&PLUGIN_LIST.state(), atoms.plugins);
    write_if_changed(&PROVIDER_LIST.state(), atoms.providers);
    write_if_changed(&MCP_SERVERS.state(), atoms.mcp_servers);
    write_if_changed(&MEMORY_LIST.state(), atoms.memory_entries);
    true
}

fn identity_is_current(client: Option<&AcpTuiClient>, expected: &ProjectionIdentity) -> bool {
    let current_cwd = ACTIVE_EXECUTION_CWD.state().read().clone();
    let current_session = ACTIVE_SESSION_ID.state().read().clone();
    let current_generation = client.and_then(AcpTuiClient::stable_session_identity);
    let current_scope = THREAD_BROWSER_SCOPE.get();
    let current_archived = THREAD_BROWSER_ARCHIVED.get();
    let current_page_count = THREAD_LIST_PAGE_COUNT.get();
    let current_bridge_reset = BRIDGE_RESET_COUNTER.get();
    current_cwd == expected.active_cwd
        && current_session == expected.session_id
        && current_generation == expected.session_generation
        && current_scope == expected.scope
        && current_archived == expected.archived
        && current_page_count == expected.page_count
        && current_bridge_reset == expected.bridge_reset
}

fn write_if_changed<T>(state: &Handle<T>, next: T)
where
    T: PartialEq + Send + Sync + 'static,
{
    if *state.read() != next {
        *state.write() = next;
    }
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
