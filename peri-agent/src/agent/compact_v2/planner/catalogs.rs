//! A shared inline result message has one canonical serialization and one catalog.
use std::collections::{HashMap, HashSet};

use super::{index::ResultLocation, MicroPlanningFacts};
use crate::error::AgentResult;
use crate::messages::MessageId;
use crate::session::transcript::MessageTranscript;
use peri_acp_types::context_archive::{build_catalog, ArchiveCatalog, ArchiveError};

#[derive(Default)]
pub(super) struct PreviewCatalogs {
    entries: HashMap<MessageId, Option<(ArchiveCatalog, usize)>>,
    used: HashSet<MessageId>,
    pub source_scans: usize,
}

impl PreviewCatalogs {
    pub fn reference_chars(
        &mut self,
        transcript: &MessageTranscript,
        result: &ResultLocation<'_>,
        facts: &MicroPlanningFacts,
    ) -> AgentResult<Option<usize>> {
        let id = result.entry.id();
        if !self.entries.contains_key(&id) {
            let catalog = if let Some(catalog) = transcript
                .projection_ref(id)
                .and_then(|directive| directive.archive_catalog.as_ref())
            {
                Some(catalog.clone())
            } else if let Some(scope) = &facts.archive_scope {
                self.source_scans += 1;
                match build_catalog(
                    scope.thread_id.clone(),
                    format!("result:{}", id.as_uuid()),
                    id,
                    &[result.entry.clone().into_payload()],
                    "Historical tool response message; original per-result error flags remain in canonical data. Preview is incomplete; recover through ReadContextArchive.".into(),
                ) {
                    Ok(catalog) => Some(catalog),
                    Err(ArchiveError::Unsupported) => None,
                    Err(error) => return Err(anyhow::Error::new(error).into()),
                }
            } else {
                None
            };
            let cached = catalog
                .map(|catalog| {
                    let chars = catalog.reference()?.chars().count() + 96;
                    Ok::<_, ArchiveError>((catalog, chars))
                })
                .transpose()
                .map_err(anyhow::Error::new)?;
            // Unsupported messages are cached too, so every sibling stays O(1).
            self.entries.insert(id, cached);
        }
        Ok(self.entries[&id].as_ref().map(|(_, chars)| *chars))
    }

    pub fn mark_used(&mut self, id: MessageId) {
        self.used.insert(id);
    }

    pub fn into_used(self) -> HashMap<MessageId, ArchiveCatalog> {
        let Self { entries, used, .. } = self;
        entries
            .into_iter()
            .filter(|(id, _)| used.contains(id))
            .filter_map(|(id, cached)| cached.map(|(catalog, _)| (id, catalog)))
            .collect()
    }
}
