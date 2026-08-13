//! Deep module owning SQLite navigation mutations and Registry projection.

use std::sync::Arc;

use acp_hub_proto::schema::{
    ProjectSessionSummary, ProjectSummary, SessionSummaryProjection, WorkspaceSummary,
};
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

    pub async fn restore_project_metadata(&self, id: &str) -> Result<(), ProjectServiceError> {
        Ok(self.metadata.restore_project(id).await?)
    }

    pub async fn rename_project_metadata(
        &self,
        id: &str,
        name: &str,
    ) -> Result<(), ProjectServiceError> {
        Ok(self.metadata.rename_project(id, name).await?)
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

    /// Refreshes ACP-derived titles for sessions already admitted to the hub
    /// catalog. SQLite remains authoritative; Registry is rebuilt only when an
    /// exact durable id changed, and user aliases continue to win at display.
    pub async fn refresh_acp_titles(
        &self,
        sessions: &[SessionSummaryProjection],
    ) -> Result<u64, ProjectServiceError> {
        let titles: Vec<_> = sessions
            .iter()
            .map(|session| (session.session_id.clone(), session.title.clone()))
            .collect();
        let changed = self.metadata.update_acp_titles(&titles).await?;
        let (generation, projected_generation) = self.metadata.generation().await?;
        if changed > 0 || generation != projected_generation {
            self.reproject().await?;
        }
        Ok(changed)
    }

    /// Derives a restrained Hub fallback from the first dispatched user
    /// prompt. It never mutates the ACP thread title and never outranks a user
    /// alias or a meaningful ACP-owned title.
    pub async fn seed_prompt_title(
        &self,
        acp_session_id: &str,
        prompt: &str,
    ) -> Result<bool, ProjectServiceError> {
        let Some(title) = prompt_title(prompt) else {
            return Ok(false);
        };
        let changed = self.metadata.seed_hub_title(acp_session_id, &title).await?;
        let (generation, projected_generation) = self.metadata.generation().await?;
        if changed || generation != projected_generation {
            self.reproject().await?;
        }
        Ok(changed)
    }

    pub async fn reproject(&self) -> Result<(), ProjectServiceError> {
        let snapshot = self.metadata.snapshot().await?;
        let projects = snapshot.projects.into_iter().map(project_summary).collect();
        let sessions = snapshot
            .sessions
            .into_iter()
            .filter(|session| session.origin != "legacy_hidden")
            .map(session_summary)
            .collect();
        self.registry.replace_projects(projects, sessions).await?;
        self.metadata.mark_projected(snapshot.generation).await?;
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

fn prompt_title(prompt: &str) -> Option<String> {
    const MAX_CHARS: usize = 60;
    let line = prompt.lines().find(|line| !line.trim().is_empty())?;
    let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        let mut shortened: String = prefix.chars().take(MAX_CHARS - 1).collect();
        shortened.push('…');
        Some(shortened)
    } else {
        Some(prefix)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use acp_hub_proto::schema::SessionSummaryProjection;
    use tempfile::tempdir;

    use super::{prompt_title, ProjectService};
    use crate::control::StoreSink;
    use crate::persist::metadata::MetadataStore;
    use crate::persist::{PersistConfig, Store};
    use crate::state::doc_manager::{BatchConfig, DocManager};

    #[tokio::test]
    async fn title_refresh_repairs_an_existing_projection_gap() {
        let dir = tempdir().unwrap();
        let store = Arc::new(
            Store::open(&PersistConfig {
                data_dir: dir.path().to_path_buf(),
                ..Default::default()
            })
            .unwrap(),
        );
        store.recover().await;
        let sink = Arc::new(StoreSink::new(store).await.unwrap());
        let doc = DocManager::new(BatchConfig::default(), sink);
        let metadata = Arc::new(MetadataStore::open(dir.path()).await.unwrap());
        metadata
            .create_project("p", "Demo", "/", "local")
            .await
            .unwrap();
        metadata
            .import_session("s", "p", "acp", "Old", "2026-08-13T00:00:00Z")
            .await
            .unwrap();
        let service = ProjectService::new(metadata.clone(), doc.registry());
        service.reproject().await.unwrap();

        metadata.update_acp_title("acp", "New").await.unwrap();
        let (generation, projected) = metadata.generation().await.unwrap();
        assert!(
            generation > projected,
            "fixture must contain a projection gap"
        );

        assert_eq!(
            service
                .refresh_acp_titles(&[SessionSummaryProjection {
                    session_id: "acp".into(),
                    title: "New".into(),
                    status: String::new(),
                    updated_at: String::new(),
                    cwd: "/".into(),
                    bound_chat_id: None,
                }])
                .await
                .unwrap(),
            0,
            "same title is a metadata no-op"
        );
        let (generation_after, projected_after) = metadata.generation().await.unwrap();
        assert_eq!(
            generation_after, projected_after,
            "no-op poll repairs pending projection"
        );
    }

    #[test]
    fn prompt_title_uses_the_first_meaningful_line_and_unicode_boundaries() {
        assert_eq!(
            prompt_title("\n  重构   ACP Hub 的会话目录  \nignored").as_deref(),
            Some("重构 ACP Hub 的会话目录")
        );
        let title = prompt_title(&"界".repeat(80)).unwrap();
        assert_eq!(title.chars().count(), 60);
        assert!(title.ends_with('…'));
        assert_eq!(prompt_title(" \n\t"), None);
    }
}
