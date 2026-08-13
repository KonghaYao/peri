//! Deep module owning SQLite navigation mutations and Registry projection.

use std::sync::Arc;

use acp_hub_proto::schema::{ProjectSessionSummary, ProjectSummary, WorkspaceSummary};
use thiserror::Error;

use crate::persist::metadata::{MetadataError, MetadataStore, ProjectRecord, ProjectSessionRecord};
use crate::state::registry::{RegistryError, RegistryState};

#[derive(Debug, Error)]
pub enum ProjectServiceError {
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[derive(Clone)]
pub struct ProjectService {
    metadata: Arc<MetadataStore>,
    registry: RegistryState,
}

impl ProjectService {
    pub fn new(metadata: Arc<MetadataStore>, registry: RegistryState) -> Self {
        Self { metadata, registry }
    }
    pub fn metadata(&self) -> &Arc<MetadataStore> {
        &self.metadata
    }

    pub async fn import_legacy_workspaces(&self) -> Result<(), ProjectServiceError> {
        const SOURCE: &str = "registry-workspaces-v1";
        if self.metadata.import_completed(SOURCE).await? {
            return Ok(());
        }
        let mut imported = 0i64;
        for w in self.registry.list_workspaces().await? {
            let rec = ProjectRecord {
                id: w.id,
                name: w.name,
                cwd: w.cwd,
                instance_id: "local".into(),
                created_at: w.created_at,
                updated_at: w.updated_at,
                archived_at: None,
            };
            if self.metadata.import_project(&rec).await? {
                imported += 1;
            }
        }
        self.metadata
            .mark_import_complete(SOURCE, imported, 0)
            .await?;
        self.reproject().await
    }

    /// Imports only legacy sessions whose cwd names exactly one live project.
    /// Empty/ambiguous cwd evidence is counted as skipped and never guessed.
    pub async fn import_legacy_sessions(&self) -> Result<(), ProjectServiceError> {
        const SOURCE: &str = "registry-sessions-v1-exact-cwd";
        if self.metadata.import_completed(SOURCE).await? {
            return Ok(());
        }
        let projects = self.metadata.list_projects().await?;
        let mut imported = 0i64;
        let mut skipped = 0i64;
        for session in self.registry.list_legacy_sessions().await? {
            let matches: Vec<_> = projects
                .iter()
                .filter(|p| {
                    p.archived_at.is_none() && !session.cwd.is_empty() && p.cwd == session.cwd
                })
                .collect();
            if matches.len() != 1 || session.session_id.trim().is_empty() {
                skipped += 1;
                continue;
            }
            let logical_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("acp-hub:legacy-session:{}", session.session_id).as_bytes(),
            )
            .to_string();
            if self
                .metadata
                .import_session(
                    &logical_id,
                    &matches[0].id,
                    &session.session_id,
                    &session.title,
                    &session.updated_at,
                )
                .await?
            {
                imported += 1;
            }
        }
        self.metadata
            .mark_import_complete(SOURCE, imported, skipped)
            .await?;
        self.reproject().await
    }

    pub async fn create_project(
        &self,
        id: &str,
        name: &str,
        cwd: &str,
        instance: &str,
    ) -> Result<ProjectRecord, ProjectServiceError> {
        let p = self
            .metadata
            .create_project(id, name, cwd, instance)
            .await?;
        self.reproject().await?;
        Ok(p)
    }

    pub async fn create_project_metadata(
        &self,
        id: &str,
        name: &str,
        cwd: &str,
        instance: &str,
    ) -> Result<ProjectRecord, ProjectServiceError> {
        Ok(self
            .metadata
            .create_project(id, name, cwd, instance)
            .await?)
    }

    pub async fn archive_project_metadata(&self, id: &str) -> Result<(), ProjectServiceError> {
        Ok(self.metadata.archive_project(id).await?)
    }

    pub async fn rename_session_metadata(
        &self,
        id: &str,
        name: &str,
    ) -> Result<(), ProjectServiceError> {
        Ok(self.metadata.rename_session(id, name).await?)
    }

    pub async fn archive_project(&self, id: &str) -> Result<(), ProjectServiceError> {
        self.metadata.archive_project(id).await?;
        self.reproject().await
    }

    pub async fn rename_session(&self, id: &str, name: &str) -> Result<(), ProjectServiceError> {
        self.metadata.rename_session(id, name).await?;
        self.reproject().await
    }

    pub async fn reproject(&self) -> Result<(), ProjectServiceError> {
        let projects = self
            .metadata
            .list_projects()
            .await?
            .into_iter()
            .map(project_summary)
            .collect();
        let sessions = self
            .metadata
            .list_sessions()
            .await?
            .into_iter()
            .filter(|session| session.origin != "legacy_hidden")
            .map(session_summary)
            .collect();
        self.registry.replace_projects(projects, sessions).await?;
        let (generation, _) = self.metadata.generation().await?;
        self.metadata.mark_projected(generation).await?;
        Ok(())
    }

    pub async fn mirror_legacy_workspace(
        &self,
        p: &ProjectRecord,
    ) -> Result<(), ProjectServiceError> {
        self.registry
            .upsert_workspace(WorkspaceSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                cwd: p.cwd.clone(),
                created_at: p.created_at.clone(),
                updated_at: p.updated_at.clone(),
            })
            .await?;
        Ok(())
    }
}

fn project_summary(p: ProjectRecord) -> ProjectSummary {
    ProjectSummary {
        id: p.id,
        name: p.name,
        cwd: p.cwd,
        instance_id: p.instance_id,
        created_at: p.created_at,
        updated_at: p.updated_at,
        archived_at: p.archived_at,
    }
}
fn session_summary(s: ProjectSessionRecord) -> ProjectSessionSummary {
    let title = s.display_title();
    ProjectSessionSummary {
        id: s.id,
        project_id: s.project_id,
        acp_session_id: s.acp_session_id,
        title,
        lifecycle: s.lifecycle,
        updated_at: s.updated_at,
        last_opened_at: s.last_opened_at,
        active_chat_id: s.last_chat_id,
    }
}
