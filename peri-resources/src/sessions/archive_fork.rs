//! Explicit fork materialization of frozen inherited archive grants.
//! Only authenticated catalog members are copied; parent history outside those
//! grants is never copied, and recovered bodies stay outside the active view.
use std::collections::{HashMap, HashSet};

use peri_acp_types::{
    context_archive::{
        content_digest, ArchiveCatalog, ArchiveChunkQuery, ArchiveMetadataQuery, ArchiveRoot,
        ARCHIVE_CHUNK_BYTES,
    },
    messages::MessageId,
    projection::MessageProjectionDirective,
    session_resources::{
        SessionResourceError, SessionResourceResult, SessionResources, SessionSnapshot,
    },
    store::{
        deserialize_persisted_payload, serialize_persisted_payload, MessageFlags, PersistedPayload,
    },
    thread::ThreadId,
};

fn conflict(detail: impl Into<String>) -> SessionResourceError {
    SessionResourceError::conflict(detail)
}

pub(super) async fn expand(
    resources: &dyn SessionResources,
    session: &ThreadId,
    mut snapshot: SessionSnapshot,
) -> SessionResourceResult<SessionSnapshot> {
    let grants: Vec<_> = snapshot
        .inherited
        .payloads
        .iter()
        .flat_map(|payload| {
            snapshot
                .inherited
                .flags
                .get(&payload.id())
                .into_iter()
                .flat_map(|flags| flags.projection.iter())
                .flat_map(|directive| directive.archive_roots.iter().cloned())
                .map(|root| (payload.id(), root))
        })
        .collect();
    let mut flags = std::mem::take(&mut snapshot.inherited.flags);
    flags.extend(std::mem::take(&mut snapshot.flags));
    let mut visible = std::mem::take(&mut snapshot.inherited.payloads);
    visible.append(&mut snapshot.payloads);
    let mut known: HashMap<_, _> = visible
        .iter()
        .map(|payload| (payload.id(), payload.clone()))
        .collect();
    if known.len() != visible.len() {
        return Err(conflict(
            "fork source contains duplicate inherited/own message IDs",
        ));
    }
    let mut recovered = Vec::new();
    let mut emitted: HashSet<_> = visible.iter().map(PersistedPayload::id).collect();
    let mut authorized_members: HashMap<ThreadId, HashSet<MessageId>> = HashMap::new();
    for (holder, root) in grants {
        let policy_version = flags
            .get(&holder)
            .and_then(|flags| flags.projection.as_ref())
            .map(|directive| directive.policy_version)
            .ok_or_else(|| conflict("frozen archive root has no projection owner"))?;
        authorize(resources, session, &root).await?;
        let catalogs = catalogs(resources, &root).await?;
        let mut required = HashSet::new();
        for catalog in &catalogs {
            catalog
                .validate()
                .map_err(|error| conflict(error.to_string()))?;
            for member in &catalog.members {
                required.insert(member.message_id);
                if let Some(existing) = known.get(&member.message_id) {
                    let bytes = serialize_persisted_payload(existing)
                        .map_err(|error| conflict(error.to_string()))?;
                    if content_digest(bytes.as_bytes()) != member.content_digest {
                        return Err(conflict(
                            "frozen fork member differs from its authorized catalog",
                        ));
                    }
                    continue;
                }
                let mut bytes = Vec::new();
                for (index, digest) in member.chunk_digests.iter().enumerate() {
                    let chunk = resources
                        .read_archive_chunk(
                            &root.source_thread_id,
                            &ArchiveChunkQuery {
                                message_id: member.message_id,
                                committed_tail: root.committed_tail,
                                offset: (index * ARCHIVE_CHUNK_BYTES) as u64,
                                max_bytes: ARCHIVE_CHUNK_BYTES as u32,
                            },
                        )
                        .await?;
                    if chunk.total_bytes != member.canonical_bytes
                        || content_digest(&chunk.bytes) != *digest
                    {
                        return Err(conflict("fork archive member digest mismatch"));
                    }
                    bytes.extend(chunk.bytes);
                }
                if content_digest(&bytes) != member.content_digest {
                    return Err(conflict("fork archive member digest mismatch"));
                }
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| conflict("fork archive canonical payload is not UTF-8"))?;
                let payload = deserialize_persisted_payload(text)
                    .map_err(|error| conflict(error.to_string()))?;
                if payload.id() != member.message_id {
                    return Err(conflict("fork archive member identity mismatch"));
                }
                known.insert(payload.id(), payload);
                flags.insert(
                    member.message_id,
                    MessageFlags {
                        excluded: true,
                        projection: Some(MessageProjectionDirective {
                            policy_version,
                            superseded_by: Some(holder),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                );
            }
            if !required.contains(&catalog.anchor_message_id) {
                return Err(conflict("fork catalog anchor is not an authorized member"));
            }
            let directive = flags
                .entry(catalog.anchor_message_id)
                .or_default()
                .projection
                .get_or_insert_with(|| MessageProjectionDirective {
                    policy_version,
                    ..Default::default()
                });
            if let Some(existing) = &directive.archive_catalog {
                if existing != catalog {
                    return Err(conflict("frozen fork catalog identity changed"));
                }
            }
            directive.archive_catalog = Some(catalog.clone());
        }
        if catalogs
            .iter()
            .flat_map(|catalog| &catalog.dependencies)
            .any(|id| !known.contains_key(id))
        {
            return Err(conflict(
                "fork archive replacement evidence is outside its frozen inherited context",
            ));
        }
        // A directory's first member can precede its result anchor. Determine the
        // lower bound through bounded identity probes, then enumerate metadata only.
        let mut first = root.first_anchor;
        for candidate in &required {
            let page = resources
                .read_archive_metadata(
                    &root.source_thread_id,
                    &ArchiveMetadataQuery {
                        first_anchor: Some(*candidate),
                        last_anchor: Some(first),
                        committed_tail: Some(root.committed_tail),
                        limit: 1,
                        ..Default::default()
                    },
                )
                .await?;
            if !page.items.is_empty() {
                first = *candidate;
            }
        }
        let mut after = None;
        let mut ordered = Vec::new();
        loop {
            let page = resources
                .read_archive_metadata(
                    &root.source_thread_id,
                    &ArchiveMetadataQuery {
                        first_anchor: Some(first),
                        after,
                        committed_tail: Some(root.committed_tail),
                        limit: 8,
                        ..Default::default()
                    },
                )
                .await?;
            for item in page.items {
                if required.contains(&item.message_id) {
                    ordered.push(item.message_id);
                }
            }
            match page.next_after {
                Some(next) if Some(next) != after => after = Some(next),
                Some(_) => return Err(conflict("fork archive pagination did not advance")),
                None => break,
            }
        }
        if ordered.len() != required.len() {
            return Err(conflict("fork archive member fell outside the frozen tail"));
        }
        if ordered.is_empty() {
            return Err(conflict(
                "frozen archive root contains no recoverable catalog",
            ));
        }
        for id in ordered {
            if emitted.insert(id) {
                recovered.push(known[&id].clone());
            }
        }
        authorized_members
            .entry(root.source_thread_id.clone())
            .or_default()
            .extend(required);
    }
    recovered.extend(visible);
    let positions: HashMap<_, _> = recovered
        .iter()
        .enumerate()
        .map(|(index, payload)| (payload.id(), index))
        .collect();
    // The target owns a finite copied history. Its directory tail ends at the last
    // copied authorized member, so no uncopied parent identity survives remapping.
    for flag in flags.values_mut() {
        if let Some(directive) = flag.projection.as_mut() {
            for root in &mut directive.archive_roots {
                if let Some(tail) = authorized_members
                    .get(&root.source_thread_id)
                    .and_then(|members| members.iter().max_by_key(|id| positions.get(id)))
                {
                    root.committed_tail = *tail;
                }
            }
        }
    }
    snapshot.payloads = recovered;
    snapshot.flags = flags;
    Ok(snapshot)
}

async fn authorize(
    resources: &dyn SessionResources,
    session: &ThreadId,
    root: &ArchiveRoot,
) -> SessionResourceResult<()> {
    let first = resources
        .inherited_archive_root(session, &root.source_thread_id, root.first_anchor)
        .await?;
    let last = resources
        .inherited_archive_root(session, &root.source_thread_id, root.last_anchor)
        .await?;
    if first != last {
        return Err(conflict(
            "fork archive endpoints have different frozen grants",
        ));
    }
    let bound = resources
        .read_archive_metadata(
            &root.source_thread_id,
            &ArchiveMetadataQuery {
                anchor: Some(root.committed_tail),
                committed_tail: Some(first.committed_tail),
                limit: 1,
                ..Default::default()
            },
        )
        .await?;
    if bound.items.len() != 1 {
        return Err(conflict("fork archive tail exceeds its frozen grant"));
    }
    Ok(())
}

async fn catalogs(
    resources: &dyn SessionResources,
    root: &ArchiveRoot,
) -> SessionResourceResult<Vec<ArchiveCatalog>> {
    let mut result = Vec::new();
    let mut after = None;
    loop {
        let page = resources
            .read_archive_metadata(
                &root.source_thread_id,
                &ArchiveMetadataQuery {
                    first_anchor: Some(root.first_anchor),
                    last_anchor: Some(root.last_anchor),
                    after,
                    committed_tail: Some(root.committed_tail),
                    limit: 8,
                    ..Default::default()
                },
            )
            .await?;
        for item in page.items {
            if let Some(catalog) = item
                .projection
                .and_then(|directive| directive.archive_catalog)
            {
                if catalog.source_thread_id != root.source_thread_id
                    || catalog.anchor_message_id != item.message_id
                {
                    return Err(conflict("fork archive catalog scope mismatch"));
                }
                result.push(catalog);
            }
        }
        match page.next_after {
            Some(next) if Some(next) != after => after = Some(next),
            Some(_) => return Err(conflict("fork archive pagination did not advance")),
            None => break,
        }
    }
    Ok(result)
}
