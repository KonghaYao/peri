//! Deployment supplied identity and workspace observation for remote storage.

use std::path::{Component, Path, PathBuf};

use anyhow::{ensure, Result};

/// The workspace observed by a remote Session host. A virtual workspace has
/// no local filesystem requirement; its root and machine ID must remain stable
/// across host restarts to regain execution eligibility.
#[derive(Clone, Debug)]
pub enum RemoteWorkspaceEnvironment {
    Native,
    #[non_exhaustive]
    Virtual {
        machine_id: String,
        root: PathBuf,
    },
}

impl RemoteWorkspaceEnvironment {
    pub fn virtual_workspace(machine_id: &str, root: PathBuf) -> Result<Self> {
        let machine_id = uuid::Uuid::parse_str(machine_id)?.to_string();
        ensure!(
            root.is_absolute(),
            "virtual workspace root must be absolute"
        );
        ensure!(
            root.components().all(|part| matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )),
            "virtual workspace root must be normalized"
        );
        Ok(Self::Virtual { machine_id, root })
    }

    pub(crate) fn machine_id(&self) -> Result<String> {
        match self {
            Self::Native => Ok(crate::sessions::machine::current()?.to_owned()),
            Self::Virtual { machine_id, .. } => Ok(machine_id.clone()),
        }
    }

    pub(crate) fn validated(self) -> Result<Self> {
        match self {
            Self::Native => Ok(Self::Native),
            Self::Virtual { machine_id, root } => Self::virtual_workspace(&machine_id, root),
        }
    }

    pub(crate) fn contains(&self, cwd: &Path) -> bool {
        match self {
            Self::Native => false,
            Self::Virtual { root, .. } => {
                cwd == root
                    || cwd.strip_prefix(root).is_ok_and(|relative| {
                        relative
                            .components()
                            .all(|part| matches!(part, Component::Normal(_)))
                    })
            }
        }
    }
}
