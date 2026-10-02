//! 旧会话到 v2 Workspace 的纯归属规划；不读取当前文件系统，也不授予执行资格。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{WorkspaceId, WorkspacePathSource};
use sqlx::SqliteConnection;

use super::discovery::Discovery;

#[derive(Clone, Debug)]
pub(crate) struct LegacyRegistration {
    pub id: WorkspaceId,
    pub root: PathBuf,
    pub path_source: WorkspacePathSource,
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

/// 在旧表仍完整时读取迁移输入。执行登记的 `discovery` 只是最后观测值，
/// 这里仅用它判断路径来源，绝不把它标成创建时的执行快照。
pub(crate) async fn read_local_plan(connection: &mut SqliteConnection) -> Result<StorageV2Plan> {
    let registration_rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, root, discovery FROM workspaces")
            .fetch_all(&mut *connection)
            .await?;
    let mut registrations = Vec::with_capacity(registration_rows.len());
    for (id, root, discovery) in registration_rows {
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
        });
    }
    let rows: Vec<(String, Option<String>, String, String, Option<String>)> = sqlx::query_as(
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
    plan_local_workspaces(&sessions, &registrations)
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
    if !path.is_absolute() && !windows_drive && !windows_unc {
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
