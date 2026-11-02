//! Parent-authorized, read-only access to a descendant's canonical timeline.
//!
//! A child need not have an ACP SessionState. One bounded ticket lives on the
//! parent; no execution context, transcript, or persistent run archive is created.
//! This is a child timeline, never an assertion of exact historical run grouping.

use serde::Deserialize;

use super::*;

const MAX_ANCESTOR_HOPS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChildLiveStamp {
    generation: u64,
    structure_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerStamp {
    parent_thread_id: String,
    generation: u64,
    structure_version: u64,
    child_live: Option<ChildLiveStamp>,
}

#[derive(Clone)]
pub(super) struct ChildHistoryScope {
    child_thread_id: String,
    byte_budget: usize,
    owner: OwnerStamp,
    issued: IssuedWindowTicket,
}

impl ChildHistoryScope {
    #[cfg(test)]
    pub(super) fn force_oversize_overflow(&self) {
        self.issued.force_oversize_overflow();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WindowParams {
    parent_session_id: String,
    child_thread_id: String,
    history: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BodyParams {
    parent_session_id: String,
    child_thread_id: String,
    entry_id: String,
    snapshot_token: String,
    read_ticket: String,
    #[serde(default)]
    source: Option<HistorySource>,
    #[serde(default)]
    offset: u64,
    max_bytes: u32,
}

fn validate_ids(parent: &str, child: &str) -> Result<(), AcpError> {
    if parent.is_empty() || child.is_empty() || parent.len() > 256 || child.len() > 256 {
        return Err(invalid(
            "parentSessionId and childThreadId must be bounded, nonempty IDs",
        ));
    }
    if parent == child {
        return Err(invalid("child history requires a distinct child thread"));
    }
    Ok(())
}

fn own_source(source: &HistorySource) -> Result<(), AcpError> {
    if source.is_own() {
        Ok(())
    } else {
        Err(invalid(
            "child history currently supports only the child's own timeline",
        ))
    }
}

fn owner_stamp(
    states: &std::collections::HashMap<String, SessionState>,
    parent: &str,
    child: &str,
) -> Result<OwnerStamp, AcpError> {
    let parent = state::readable_state(states, parent)?;
    let child_live = if states.contains_key(child) {
        let live = state::readable_state(states, child)?;
        if live.thread_id != child {
            return Err(invalidated(
                "child session no longer names the requested thread",
            ));
        }
        Some(ChildLiveStamp {
            generation: live.session_generation,
            structure_version: live.structure_version,
        })
    } else {
        None
    };
    Ok(OwnerStamp {
        parent_thread_id: parent.thread_id.clone(),
        generation: parent.session_generation,
        structure_version: parent.structure_version,
        child_live,
    })
}

async fn authorize(cfg: &AcpServerConfig, owner: &OwnerStamp, child: &str) -> Result<(), AcpError> {
    // Nested Started events retain their own child identity. Walk only canonical
    // ownership metadata, with a fixed work/memory bound; never load a runtime.
    let mut current = child.to_owned();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..MAX_ANCESTOR_HOPS {
        if current.is_empty() || current.len() > 256 || !visited.insert(current.clone()) {
            return Err(ticket_invalid("child ancestry is malformed or cyclic"));
        }
        let meta = cfg
            .session_resources
            .load_session_meta(&current)
            .await
            .map_err(resource_window_error)?;
        if meta.id != current {
            return Err(ticket_invalid("child ancestry returned a different thread"));
        }
        match meta.parent_thread_id {
            Some(parent) if parent == owner.parent_thread_id => return Ok(()),
            Some(parent) => current = parent,
            None => {
                return Err(ticket_invalid(
                    "requested thread is outside the parent tree",
                ))
            }
        }
    }
    Err(ticket_invalid("child ancestry exceeds the read limit"))
}

/// The connection capability is checked by ServerLoop before entering this route.
pub(crate) async fn handle(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    params: &Value,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let params: WindowParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid("invalid child history request"))?;
    validate_ids(&params.parent_session_id, &params.child_thread_id)?;
    let request = HistoryWindowRequest::decode(&params.history).map_err(window_error)?;
    match request {
        HistoryWindowRequest::Open(request) => {
            open(cfg, sessions, caps, &params, request, connection).await
        }
        request => read_window(cfg, sessions, caps, &params, request, connection).await,
    }
}

async fn open(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    params: &WindowParams,
    request: HistoryWindowOpenRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let plan = request.validate().map_err(window_error)?;
    let child = &params.child_thread_id;
    let anchor = match &plan.anchor {
        peri_acp_types::history_window::HistoryWindowAnchor::Latest => None,
        peri_acp_types::history_window::HistoryWindowAnchor::Entry { entry_id, source } => {
            own_source(source)?;
            Some(state::parse_anchor(entry_id, child)?)
        }
    };
    let (owner, epoch) = {
        let states = sessions.lock().await;
        let owner = owner_stamp(&states, &params.parent_session_id, child)?;
        let epoch = states[&params.parent_session_id]
            .history_window
            .child_ticket_epoch;
        (owner, epoch)
    };
    authorize(cfg, &owner, child).await?;
    let bounds = cfg
        .session_resources
        .history_window_bounds(child)
        .await
        .map_err(resource_window_error)?;
    let metadata = HistoryWindowMetadata {
        session_id: child.clone(),
        session_generation: owner.generation.to_string(),
        structure_version: owner
            .child_live
            .as_ref()
            .map_or(0, |live| live.structure_version),
        snapshot_token: uuid::Uuid::now_v7().to_string(),
        committed_tail: bounds.own_latest,
        checkpoint_id: bounds.checkpoint.map(|checkpoint| checkpoint.checkpoint_id),
    };
    let prepared = HistoryWindowCoordinator::prepare(
        metadata,
        HistorySource::Own,
        connection,
        bounds.inherited.snapshot().is_some(),
        None,
    );
    let page_plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Open,
        source: HistorySource::Own,
        anchor,
        direction: plan.direction,
        include_anchor: anchor.is_some(),
        before_limit: plan.before_limit,
        after_limit: plan.after_limit,
        byte_budget: plan.byte_budget,
        tail: HistoryWindowTailExpectation::Fixed(prepared.metadata.committed_tail.clone()),
    };
    let rows = cfg
        .session_resources
        .load_history_window_page(child, &page_plan)
        .await
        .map_err(resource_window_error);
    let rows = filter_rows(cfg, child, &prepared.opaque, rows).await?;
    let oversize = wire::oversize_descriptors(Some(&rows));
    authorize(cfg, &owner, child).await?;
    let scope = {
        let mut states = sessions.lock().await;
        if owner_stamp(&states, &params.parent_session_id, child)? != owner {
            return Err(invalidated("parent or child changed while opening history"));
        }
        let coordinator = &mut states
            .get_mut(&params.parent_session_id)
            .unwrap()
            .history_window;
        if coordinator.child_ticket_epoch != epoch {
            return Err(ticket_invalid("child history window changed while opening"));
        }
        coordinator.child_ticket_epoch = epoch.saturating_add(1);
        let issued = prepared.issue(coordinator.child_ticket_epoch);
        issued.remember_oversize(&oversize);
        let scope = ChildHistoryScope {
            child_thread_id: child.clone(),
            byte_budget: plan.byte_budget,
            owner,
            issued,
        };
        coordinator.child_scope = Some(scope.clone());
        scope
    };
    Ok(response(&params.parent_session_id, &scope, rows, caps))
}

async fn prepare_read(
    sessions: &SharedSessions,
    parent: &str,
    child: &str,
    connection_id: &str,
    snapshot: &str,
    ticket: &str,
) -> Result<ChildHistoryScope, AcpError> {
    let states = sessions.lock().await;
    let owner = owner_stamp(&states, parent, child)?;
    let scope = states[parent]
        .history_window
        .child_scope
        .as_ref()
        .ok_or_else(|| ticket_invalid("child history scope was revoked or replaced"))?;
    if scope.child_thread_id != child || scope.owner != owner {
        return Err(invalidated(
            "child history scope no longer matches the live parent or child",
        ));
    }
    if scope.issued.binding.connection_id != connection_id
        || scope.issued.opaque.as_str() != ticket
        || scope.issued.metadata.snapshot_token != snapshot
    {
        return Err(ticket_invalid(
            "child history ticket does not match this connection and snapshot",
        ));
    }
    scope
        .issued
        .binding
        .validate_at(now_unix_ms(), &scope.issued.metadata, &HistorySource::Own)
        .map_err(window_error)?;
    Ok(scope.clone())
}

async fn finalize_read(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    parent: &str,
    scope: &ChildHistoryScope,
    oversize: &[(peri_acp_types::messages::MessageId, u64)],
) -> Result<(), AcpError> {
    authorize(cfg, &scope.owner, &scope.child_thread_id).await?;
    let current = prepare_read(
        sessions,
        parent,
        &scope.child_thread_id,
        &scope.issued.binding.connection_id,
        &scope.issued.metadata.snapshot_token,
        scope.issued.opaque.as_str(),
    )
    .await?;
    if current.issued.ticket_epoch != scope.issued.ticket_epoch {
        return Err(ticket_invalid(
            "child history scope changed during the read",
        ));
    }
    current.issued.remember_oversize(oversize);
    Ok(())
}

async fn read_window(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    params: &WindowParams,
    request: HistoryWindowRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let parent = &params.parent_session_id;
    let child = &params.child_thread_id;
    let (scope, plan) = match request {
        HistoryWindowRequest::Page(request) => {
            let direction = request.validate().map_err(window_error)?;
            let scope = prepare_read(
                sessions,
                parent,
                child,
                &connection.id,
                &request.snapshot_token,
                &request.read_ticket,
            )
            .await?;
            let raw = if direction == HistoryWindowDirection::Before {
                request.before_cursor
            } else {
                request.after_cursor
            }
            .ok_or_else(|| invalid("exactly one cursor is required"))?;
            let cursor = HistoryWindowCursor::decode(&raw, child).map_err(window_error)?;
            if cursor.direction != direction {
                return Err(invalid("cursor direction does not match the request"));
            }
            cursor
                .verify_against(&scope.issued.metadata, &HistorySource::Own)
                .map_err(window_error)?;
            let plan = HistoryWindowPagePlan::page(
                HistorySource::Own,
                cursor.anchor,
                direction,
                request.limit as usize,
                scope.byte_budget,
                scope.issued.binding.committed_tail.clone(),
            );
            (scope, plan)
        }
        HistoryWindowRequest::Seek(request) => {
            let (before_limit, after_limit) = request.validate().map_err(window_error)?;
            own_source(&request.anchor.source)?;
            let anchor = state::parse_anchor(&request.anchor.entry_id, child)?;
            let scope = prepare_read(
                sessions,
                parent,
                child,
                &connection.id,
                &request.snapshot_token,
                &request.read_ticket,
            )
            .await?;
            let plan = HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Seek,
                source: HistorySource::Own,
                anchor: Some(anchor),
                direction: HistoryWindowDirection::Around,
                include_anchor: true,
                before_limit,
                after_limit,
                byte_budget: scope.byte_budget,
                tail: HistoryWindowTailExpectation::Fixed(
                    scope.issued.binding.committed_tail.clone(),
                ),
            };
            (scope, plan)
        }
        HistoryWindowRequest::Open(_) => return Err(invalid("open must create a new child scope")),
    };
    authorize(cfg, &scope.owner, child).await?;
    let rows = cfg
        .session_resources
        .load_history_window_page(child, &plan)
        .await
        .map_err(resource_window_error);
    let rows = filter_rows(cfg, child, &scope.issued.opaque, rows).await?;
    let oversize = wire::oversize_descriptors(Some(&rows));
    finalize_read(cfg, sessions, parent, &scope, &oversize).await?;
    Ok(response(parent, &scope, rows, caps))
}

fn response(
    parent: &str,
    scope: &ChildHistoryScope,
    rows: HistoryWindowRows,
    caps: &PeriCaps,
) -> Value {
    let mut value = wire::response(
        &scope.child_thread_id,
        &scope.child_thread_id,
        &scope.issued.metadata,
        &scope.issued,
        &HistorySource::Own,
        rows,
        caps,
    );
    value["parentSessionId"] = json!(parent);
    value["scope"] = json!("child");
    value
}

pub(crate) async fn handle_body(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    params: &Value,
    connection_id: &str,
) -> Result<Value, AcpError> {
    let params: BodyParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid("invalid child history body request"))?;
    validate_ids(&params.parent_session_id, &params.child_thread_id)?;
    if let Some(source) = &params.source {
        own_source(source)?;
    }
    if !(4..=HISTORY_WINDOW_MAX_CHUNK_SIZE).contains(&params.max_bytes) {
        return Err(invalid("maxBytes is out of range"));
    }
    let entry = state::parse_anchor(&params.entry_id, &params.child_thread_id)?;
    let scope = prepare_read(
        sessions,
        &params.parent_session_id,
        &params.child_thread_id,
        connection_id,
        &params.snapshot_token,
        &params.read_ticket,
    )
    .await?;
    authorize(cfg, &scope.owner, &params.child_thread_id).await?;
    let issued = &scope.issued;
    let tail = issued.binding.committed_tail.as_ref();
    if tail.is_none() || !issued.remembers_oversize(entry) {
        return Err(anchor_unavailable(
            "entry is not an oversize descriptor of this snapshot",
        ));
    }
    if issued.oversize_overflowed() && !is_oversize_descriptor(cfg, &scope, entry).await? {
        return Err(anchor_unavailable(
            "entry is not an oversize descriptor of this snapshot",
        ));
    }
    if !stream_row(
        cfg,
        &params.child_thread_id,
        tail,
        &issued.opaque,
        entry,
        super::child_display::visible,
    )
    .await?
    {
        return Err(anchor_unavailable("entry is not visible to this client"));
    }
    let chunk = stream_row(
        cfg,
        &scope.child_thread_id,
        tail,
        &issued.opaque,
        entry,
        move |reader| super::child_display::project(reader, params.offset, params.max_bytes),
    )
    .await?;
    finalize_read(cfg, sessions, &params.parent_session_id, &scope, &[]).await?;
    let mut value = json!({
        "format":"display_text", "entryId":params.entry_id, "source":"own",
        "offset":params.offset, "bytes":chunk.bytes, "nextOffset":chunk.next_offset,
        "complete":chunk.complete,
    });
    value["scope"] = json!("child");
    value["parentSessionId"] = json!(params.parent_session_id);
    value["childThreadId"] = json!(params.child_thread_id);
    value["snapshotToken"] = json!(params.snapshot_token);
    value["committedTail"] = json!(scope
        .issued
        .metadata
        .committed_tail
        .as_ref()
        .map(|tail| tail.encode()));
    Ok(value)
}

async fn is_oversize_descriptor(
    cfg: &AcpServerConfig,
    scope: &ChildHistoryScope,
    entry_id: peri_acp_types::messages::MessageId,
) -> Result<bool, AcpError> {
    use peri_acp_types::session_resources::HistoryWindowEntry;
    let plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: HistorySource::Own,
        anchor: Some(entry_id),
        direction: HistoryWindowDirection::After,
        include_anchor: true,
        before_limit: 0,
        after_limit: 1,
        // Preserve the budget under which the descriptor was originally issued.
        byte_budget: scope.byte_budget,
        tail: HistoryWindowTailExpectation::Fixed(scope.issued.binding.committed_tail.clone()),
    };
    let rows = cfg
        .session_resources
        .load_history_window_page(&scope.child_thread_id, &plan)
        .await
        .map_err(resource_window_error)?;
    inherited::validate_rows(&HistorySource::Own, &rows)?;
    Ok(rows.entries.iter().any(|entry| {
        matches!(entry,
            HistoryWindowEntry::Oversized { entry_id: found, .. } if *found == entry_id
        )
    }))
}

struct DisplayReader {
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    current: std::io::Cursor<Vec<u8>>,
}

impl std::io::Read for DisplayReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let count = std::io::Read::read(&mut self.current, output)?;
            if count != 0 || output.is_empty() {
                return Ok(count);
            }
            match self.receiver.blocking_recv() {
                Some(bytes) => self.current = std::io::Cursor::new(bytes),
                None => return Ok(0),
            }
        }
    }
}

/// Offsets belong to the projected UTF-8 stream. Re-reading a later offset scans
/// this row from the beginning; neither raw JSON nor a full string is retained.
async fn stream_row<T: Send + 'static>(
    cfg: &AcpServerConfig,
    child: &str,
    tail: Option<&HistoryCommittedTail>,
    ticket: &HistoryReadTicket,
    entry: peri_acp_types::messages::MessageId,
    parse: impl FnOnce(DisplayReader) -> std::io::Result<T> + Send + 'static,
) -> Result<T, AcpError> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let parsing = tokio::task::spawn_blocking(move || {
        parse(DisplayReader {
            receiver,
            current: std::io::Cursor::new(Vec::new()),
        })
    });
    let child = child.to_owned();
    let expected = HistoryEntryId::new(&child, entry).encode();
    let mut raw_offset = 0;
    let mut failure = None;
    loop {
        let chunk = cfg
            .session_resources
            .load_history_body_chunk(
                &child,
                HistorySource::Own,
                entry,
                raw_offset,
                HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
                tail,
                ticket,
            )
            .await
            .map_err(resource_window_error);
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        if let Err(error) = chunk.validate(&expected, raw_offset, HISTORY_WINDOW_DEFAULT_CHUNK_SIZE)
        {
            failure = Some(window_error(error));
            break;
        }
        let next = chunk.next_offset;
        if sender.send(chunk.bytes).await.is_err() {
            break;
        }
        match next {
            Some(next) => raw_offset = next,
            None => break,
        }
    }
    drop(sender);
    let result = parsing
        .await
        .map_err(|_| anchor_unavailable("child display projection failed"))?;
    if let Some(error) = failure {
        return Err(error);
    }
    result.map_err(|error| {
        window_error(HistoryWindowError::ReadFailed {
            detail: if error.kind() == std::io::ErrorKind::Unsupported {
                "child display projection unsupported for this payload"
            } else {
                "child display projection is malformed or its offset is invalid"
            }
            .into(),
        })
    })
}

async fn filter_rows(
    cfg: &AcpServerConfig,
    child: &str,
    ticket: &HistoryReadTicket,
    rows: Result<HistoryWindowRows, AcpError>,
) -> Result<HistoryWindowRows, AcpError> {
    use peri_acp_types::session_resources::HistoryWindowEntry;
    let mut rows = rows?;
    inherited::validate_rows(&HistorySource::Own, &rows)?;
    let mut visible = Vec::new();
    for (entry, _) in wire::oversize_descriptors(Some(&rows)) {
        if stream_row(
            cfg,
            child,
            rows.committed_tail.as_ref(),
            ticket,
            entry,
            super::child_display::visible,
        )
        .await?
        {
            visible.push(entry);
        }
    }
    rows.entries.retain(|entry| match entry {
        HistoryWindowEntry::Oversized { entry_id, .. } => visible.contains(entry_id),
        HistoryWindowEntry::Loaded(item) => item.payload.displayable_to_client(),
    });
    Ok(rows)
}
