//! 旧会话到 v2 Workspace 的纯归属规划；不读取当前文件系统，也不授予执行资格。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{WorkspaceId, WorkspacePathSource};
#[cfg(not(target_os = "emscripten"))]
use sqlx::SqliteConnection;

#[cfg(not(target_os = "emscripten"))]
use super::discovery::Discovery;

/// 旧登记（压缩前形状里 `workspaces` 的一行）：归属身份 + 最后观测证据。
///
/// 证据三列随归属行一起搬到当前形状；`discovery` 同时是绑定行的证据来源，
/// 因此它不只是展示数据，缺失会让升级 fail-closed。
#[derive(Clone, Debug)]
pub(crate) struct LegacyRegistration {
    pub id: WorkspaceId,
    pub root: PathBuf,
    pub path_source: WorkspacePathSource,
    pub project_id: String,
    pub root_identity: String,
    pub discovery: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LegacySession {
    pub id: ThreadId,
    pub parent_id: Option<ThreadId>,
    pub machine_id: String,
    pub cwd: PathBuf,
    pub execution_workspace_id: Option<WorkspaceId>,
    /// Remote v11 lacks the local execution registry. A verified inverse of
    /// saved cwd + relative cwd supplies grouping only, never execution proof.
    pub derived_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedWorkspace {
    pub id: WorkspaceId,
    pub machine_id: String,
    pub path: PathBuf,
    pub path_source: WorkspacePathSource,
}

#[derive(Clone, Debug)]
pub(crate) struct StorageV2Plan {
    pub workspaces: Vec<PlannedWorkspace>,
    pub session_workspace_ids: HashMap<ThreadId, WorkspaceId>,
}

/// 本机迁移输入：归属规划 + 规划所用的旧事实（会话与登记行）。
#[derive(Clone, Debug)]
pub(crate) struct LocalMigrationInput {
    pub plan: StorageV2Plan,
    pub sessions: Vec<LegacySession>,
    pub registrations: Vec<LegacyRegistration>,
}

/// 旧表会话行：`(thread id, parent id, cwd, machine id, workspace id)`。
#[cfg(not(target_os = "emscripten"))]
type LegacySessionRow = (String, Option<String>, String, String, Option<String>);

/// 在旧表仍完整时读取迁移输入。执行登记的 `discovery` 只是最后观测值，
/// 这里仅用它判断路径来源，绝不把它标成创建时的执行快照。
#[cfg(not(target_os = "emscripten"))]
pub(crate) async fn read_local_plan(
    connection: &mut SqliteConnection,
) -> Result<LocalMigrationInput> {
    let registration_rows: Vec<(String, String, String, String, String)> =
        sqlx::query_as("SELECT id, project_id, root, root_identity, discovery FROM workspaces")
            .fetch_all(&mut *connection)
            .await?;
    let mut registrations = Vec::with_capacity(registration_rows.len());
    for (id, project_id, root, root_identity, discovery) in registration_rows {
        let observed: Discovery = serde_json::from_str(&discovery)?;
        if observed.root != Path::new(&root) {
            bail!("legacy execution registration root differs from discovery");
        }
        registrations.push(LegacyRegistration {
            id: id.parse()?,
            root: root.into(),
            path_source: if observed.common_dir.is_some() || observed.private_dir.is_some() {
                WorkspacePathSource::Discovered
            } else {
                WorkspacePathSource::Unverified
            },
            project_id,
            root_identity,
            discovery,
        });
    }
    let rows: Vec<LegacySessionRow> = sqlx::query_as(
        "SELECT t.id, t.parent_thread_id, t.cwd, e.machine_id, b.workspace_id
         FROM threads t
         JOIN session_environments e ON e.thread_id = t.id
         LEFT JOIN session_bindings b ON b.thread_id = t.id",
    )
    .fetch_all(&mut *connection)
    .await?;
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads")
        .fetch_one(&mut *connection)
        .await?;
    if rows.len() != usize::try_from(total.0)? {
        bail!("legacy session environment is incomplete");
    }
    let mut sessions = Vec::with_capacity(rows.len());
    for (id, parent_id, cwd, machine_id, execution_workspace_id) in rows {
        sessions.push(LegacySession {
            id,
            parent_id,
            machine_id,
            cwd: cwd.into(),
            execution_workspace_id: execution_workspace_id.map(|id| id.parse()).transpose()?,
            derived_root: None,
        });
    }
    plan_local_workspaces(&sessions, &registrations).map(|plan| LocalMigrationInput {
        plan,
        sessions,
        registrations,
    })
}

/// 当前形状的归属行（迁移落库的完整内容）：归属身份 + 旧登记证据（可有可无）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedWorkspaceRow {
    pub id: WorkspaceId,
    pub machine_id: String,
    pub path: PathBuf,
    pub path_source: WorkspacePathSource,
    pub project_id: Option<String>,
    pub identity: Option<String>,
    pub discovery: Option<String>,
}

/// 旧绑定行（迁移输入）。
#[derive(Clone, Debug)]
pub(crate) struct LegacyBindingRow {
    pub thread_id: ThreadId,
    pub schema_version: i64,
    pub project_id: String,
    /// 指向旧登记，不是归属行。
    pub workspace_id: WorkspaceId,
    pub relative_cwd: String,
}

/// 当前形状的绑定行：归属收敛到会话归属行，证据来自旧登记。
#[derive(Clone, Debug)]
pub(crate) struct PlannedBindingRow {
    pub thread_id: ThreadId,
    pub schema_version: i64,
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub relative_cwd: String,
    pub discovery_snapshot: String,
    pub evidence_origin: &'static str,
}

/// 压缩前形状的绑定证据来源：旧登记的最后观测，不是创建时的执行快照。
pub(crate) const LEGACY_EVIDENCE_ORIGIN: &str = "legacy_last_observation";

/// 旧机器 id 的展示名与身份种类：uuid 字面量是「已知机器」（本机铸造的 id），
/// 其余是旧库留下的未知机器。规则与规划同源，两端搬运共用一份判定。
pub(crate) fn machine_identity(machine_id: &str) -> (&'static str, &'static str) {
    let known =
        uuid::Uuid::parse_str(machine_id).is_ok_and(|parsed| parsed.to_string() == machine_id);
    if known {
        ("我的电脑", "known")
    } else {
        ("旧机器", "legacy_unknown")
    }
}

/// `path_source` 的列取值：规则属于领域（归属来源的种类），两端写同一列时共用一份派生。
pub(crate) fn path_source_name(source: WorkspacePathSource) -> &'static str {
    match source {
        WorkspacePathSource::Discovered => "discovered",
        WorkspacePathSource::DerivedLegacy => "derived_legacy",
        WorkspacePathSource::Unverified => "unverified",
    }
}

/// 归属行的完整规划：规划出的归属行 + 旧登记的证据（同名登记优先，其次同路径的
/// `id` 最小登记）+ 无会话引用但仍要保留的登记（`(machine_id, path)` 只能有一行，
/// 因此同路径只并入 `id` 最小的一条）。
///
/// `machines` 是本次迁移会写进归属表的机器全集（本机含当前机器）：保留行取不到
/// 会话归属时退到它的 `id` 最小者，与「没有会话引用时取该库的第一台机器」同义。
pub(crate) fn plan_workspace_rows(
    plan: &StorageV2Plan,
    sessions: &[LegacySession],
    registrations: &[LegacyRegistration],
    machines: &std::collections::BTreeSet<String>,
) -> Result<Vec<PlannedWorkspaceRow>> {
    let by_id: HashMap<_, _> = registrations
        .iter()
        .map(|registration| (registration.id, registration))
        .collect();
    let mut by_root: HashMap<&Path, &LegacyRegistration> = HashMap::new();
    for registration in registrations {
        by_root
            .entry(registration.root.as_path())
            .and_modify(|current| {
                if registration.id.to_string() < current.id.to_string() {
                    *current = registration;
                }
            })
            .or_insert(registration);
    }

    let mut rows = Vec::with_capacity(plan.workspaces.len());
    for workspace in &plan.workspaces {
        // 归属行的 id 可能沿用旧登记 UUID，也可能是铸造值（同一旧 UUID 被别的机器
        // 占用时）；前者按 id 取证据，其余按路径取同一路径的最近观测。
        let evidence = by_id
            .get(&workspace.id)
            .copied()
            .or_else(|| by_root.get(workspace.path.as_path()).copied());
        rows.push(PlannedWorkspaceRow {
            id: workspace.id,
            machine_id: workspace.machine_id.clone(),
            path: workspace.path.clone(),
            path_source: workspace.path_source,
            project_id: evidence.map(|registration| registration.project_id.clone()),
            identity: evidence.map(|registration| registration.root_identity.clone()),
            discovery: evidence.map(|registration| registration.discovery.clone()),
        });
    }

    let mut planned_ids: HashSet<WorkspaceId> = rows.iter().map(|row| row.id).collect();
    let mut planned_paths: HashSet<PathBuf> = rows.iter().map(|row| row.path.clone()).collect();
    for registration in registrations {
        if planned_paths.contains(&registration.root) || planned_ids.contains(&registration.id) {
            continue;
        }
        if by_root.get(registration.root.as_path()).map(|it| it.id) != Some(registration.id) {
            continue;
        }
        let machine = bound_machine(registration.id, sessions, plan)
            .or_else(|| machines.iter().next().cloned());
        let Some(machine) = machine else {
            continue;
        };
        planned_ids.insert(registration.id);
        planned_paths.insert(registration.root.clone());
        rows.push(PlannedWorkspaceRow {
            id: registration.id,
            machine_id: machine,
            path: registration.root.clone(),
            path_source: WorkspacePathSource::Unverified,
            project_id: Some(registration.project_id.clone()),
            identity: Some(registration.root_identity.clone()),
            discovery: Some(registration.discovery.clone()),
        });
    }
    Ok(rows)
}

/// 引用该登记的会话所属机器：取这些会话的归属行里 `id` 最小的一台。
fn bound_machine(
    registration_id: WorkspaceId,
    sessions: &[LegacySession],
    plan: &StorageV2Plan,
) -> Option<String> {
    let owner = sessions
        .iter()
        .filter(|session| session.execution_workspace_id == Some(registration_id))
        .filter_map(|session| plan.session_workspace_ids.get(&session.id))
        .min_by_key(|workspace_id| workspace_id.to_string())?;
    plan.workspaces
        .iter()
        .find(|workspace| workspace.id == *owner)
        .map(|workspace| workspace.machine_id.clone())
}

/// 绑定行的搬运：`workspace_id` 由旧登记收敛到**会话归属行**，证据取旧登记的
/// `discovery`，来源标为最后观测。任一绑定落不到归属行、或其记录的根与归属行
/// 不同路径，都说明改写会静默改绑，直接拒绝升级（fail-closed）。
pub(crate) fn plan_binding_rows(
    bindings: &[LegacyBindingRow],
    plan: &StorageV2Plan,
    registrations: &[LegacyRegistration],
) -> Result<Vec<PlannedBindingRow>> {
    let by_id: HashMap<_, _> = registrations
        .iter()
        .map(|registration| (registration.id, registration))
        .collect();
    let owners: HashMap<_, _> = plan
        .workspaces
        .iter()
        .map(|workspace| (workspace.id, workspace.path.as_path()))
        .collect();
    let mut rows = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let Some(owner) = plan.session_workspace_ids.get(&binding.thread_id) else {
            bail!("legacy binding has no session workspace owner");
        };
        let Some(registration) = by_id.get(&binding.workspace_id) else {
            bail!("legacy binding references a missing execution registration");
        };
        if owners.get(owner).copied() != Some(registration.root.as_path()) {
            bail!("legacy binding root differs from its session workspace");
        }
        rows.push(PlannedBindingRow {
            thread_id: binding.thread_id.clone(),
            schema_version: binding.schema_version,
            project_id: binding.project_id.clone(),
            workspace_id: *owner,
            relative_cwd: binding.relative_cwd.clone(),
            discovery_snapshot: registration.discovery.clone(),
            evidence_origin: LEGACY_EVIDENCE_ORIGIN,
        });
    }
    Ok(rows)
}

/// 以旧登记保存的 root 或会话保存的 cwd 规划归属。缺失的旧登记不能由当前 pwd 补造。
/// 子会话一律继承父会话的 Workspace，且机器归属冲突、循环和缺失父链直接失败。
pub(crate) fn plan_local_workspaces(
    sessions: &[LegacySession],
    registrations: &[LegacyRegistration],
) -> Result<StorageV2Plan> {
    let registration_by_id: HashMap<_, _> = registrations
        .iter()
        .map(|registration| (registration.id, registration))
        .collect();
    if registration_by_id.len() != registrations.len() {
        bail!("duplicate legacy execution workspace id");
    }
    let session_by_id: HashMap<_, _> = sessions
        .iter()
        .map(|session| (&session.id, session))
        .collect();
    if session_by_id.len() != sessions.len() {
        bail!("duplicate session id during workspace migration");
    }
    for session in sessions {
        validate_saved_cwd(&session.cwd)?;
        if session.machine_id.is_empty() {
            bail!("legacy session has no machine identity");
        }
        if session.execution_workspace_id.is_some_and(|id| {
            !registration_by_id.contains_key(&id) && session.derived_root.is_none()
        }) {
            bail!("missing legacy execution registration");
        }
    }

    // 每个根会话先形成候选键；同机同路径归组，跨机器即使旧 UUID 相同也拆开。
    let mut groups: BTreeMap<(String, PathBuf), Vec<&LegacySession>> = BTreeMap::new();
    let mut roots = HashMap::new();
    for session in sessions
        .iter()
        .filter(|session| session.parent_id.is_none())
    {
        let (path, source) = match session.execution_workspace_id {
            Some(id) => match registration_by_id.get(&id) {
                Some(registration) => (registration.root.clone(), registration.path_source),
                None => (
                    session
                        .derived_root
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing legacy execution registration"))?,
                    WorkspacePathSource::DerivedLegacy,
                ),
            },
            None => (session.cwd.clone(), WorkspacePathSource::Unverified),
        };
        validate_saved_cwd(&path)?;
        roots.insert(session.id.clone(), (path.clone(), source));
        groups
            .entry((session.machine_id.clone(), path))
            .or_default()
            .push(session);
    }

    let mut workspaces = Vec::with_capacity(groups.len());
    let mut session_workspace_ids = HashMap::with_capacity(sessions.len());
    let mut retained_old_ids = HashSet::new();
    let mut all_old_ids: HashSet<_> = registrations
        .iter()
        .map(|registration| registration.id)
        .collect();
    all_old_ids.extend(
        sessions
            .iter()
            .filter_map(|session| session.execution_workspace_id),
    );
    for ((machine_id, path), members) in groups {
        // 旧 UUID 只可被一个目标键保留。排序后的首个键取得它，其他键铸造新 UUID。
        let candidate = members
            .iter()
            .filter_map(|session| session.execution_workspace_id)
            .filter(|id| !retained_old_ids.contains(id))
            .min_by_key(|id| id.to_string());
        let id = match candidate {
            Some(candidate) => {
                retained_old_ids.insert(candidate);
                candidate
            }
            None => loop {
                let minted = WorkspaceId::new();
                if !all_old_ids.contains(&minted) && retained_old_ids.insert(minted) {
                    break minted;
                }
            },
        };
        let path_source = members
            .iter()
            .filter_map(|session| roots.get(&session.id).map(|(_, source)| *source))
            .fold(WorkspacePathSource::Discovered, |current, source| {
                if current == WorkspacePathSource::Unverified
                    || source == WorkspacePathSource::Unverified
                {
                    WorkspacePathSource::Unverified
                } else if current == WorkspacePathSource::DerivedLegacy
                    || source == WorkspacePathSource::DerivedLegacy
                {
                    WorkspacePathSource::DerivedLegacy
                } else {
                    WorkspacePathSource::Discovered
                }
            });
        for member in members {
            session_workspace_ids.insert(member.id.clone(), id);
        }
        workspaces.push(PlannedWorkspace {
            id,
            machine_id,
            path,
            path_source,
        });
    }

    for session in sessions
        .iter()
        .filter(|session| session.parent_id.is_some())
    {
        let mut visited = HashSet::new();
        let mut cursor = session;
        while let Some(parent_id) = &cursor.parent_id {
            if !visited.insert(cursor.id.clone()) {
                bail!("cycle in legacy session parent chain");
            }
            let parent = session_by_id
                .get(parent_id)
                .ok_or_else(|| anyhow::anyhow!("missing legacy parent session"))?;
            if parent.machine_id != session.machine_id {
                bail!("child session machine conflicts with parent");
            }
            cursor = parent;
        }
        let id = session_workspace_ids
            .get(&cursor.id)
            .ok_or_else(|| anyhow::anyhow!("legacy root has no workspace assignment"))?;
        session_workspace_ids.insert(session.id.clone(), *id);
    }
    Ok(StorageV2Plan {
        workspaces,
        session_workspace_ids,
    })
}

pub(crate) fn validate_saved_cwd(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy session path is not UTF-8"))?;
    let posix_absolute = text.starts_with('/');
    let windows_drive = text.as_bytes().get(1) == Some(&b':')
        && text
            .as_bytes()
            .get(2)
            .is_some_and(|separator| *separator == b'\\' || *separator == b'/')
        && text.as_bytes()[0].is_ascii_alphabetic();
    let windows_unc = text.starts_with("\\\\")
        && text[2..]
            .split('\\')
            .filter(|part| !part.is_empty())
            .count()
            >= 2;
    if !posix_absolute && !windows_drive && !windows_unc {
        bail!("legacy session path is not absolute");
    }
    Ok(())
}

/// Infer a remote v11 root only when the saved relative path is an exact
/// suffix of the saved execution cwd. This is grouping evidence, not a Git or
/// filesystem identity check.
pub(crate) fn derive_remote_root(cwd: &str, relative: &str) -> Result<PathBuf> {
    validate_saved_cwd(Path::new(cwd))?;
    if relative.is_empty() {
        return Ok(PathBuf::from(cwd));
    }
    if relative
        .split(['/', '\\'])
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        bail!("remote binding relative cwd is invalid");
    }
    let separator = if cwd.contains('\\') { '\\' } else { '/' };
    let relative = relative.replace(['/', '\\'], &separator.to_string());
    let suffix = format!("{separator}{relative}");
    let prefix = cwd
        .strip_suffix(&suffix)
        .ok_or_else(|| anyhow::anyhow!("remote binding cwd does not match relative path"))?;
    let root = if prefix.is_empty() {
        separator.to_string()
    } else if prefix.len() == 2 && prefix.as_bytes()[1] == b':' {
        format!("{prefix}{separator}")
    } else {
        prefix.to_owned()
    };
    validate_saved_cwd(Path::new(&root))?;
    Ok(PathBuf::from(root))
}

#[cfg(test)]
#[path = "storage_v2_plan_test.rs"]
mod tests;
