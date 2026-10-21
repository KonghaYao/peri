//! Archive-aware model projection. Source messages remain immutable.
use super::*;
use peri_acp_types::context_archive::{ArchiveCatalog, ArchiveReference, ArchiveRoot};
use sha2::{Digest, Sha256};

fn invalid(detail: impl std::fmt::Display) -> crate::error::AgentError {
    crate::error::AgentError::LlmError(format!("invalid context projection: {detail}"))
}

pub(super) fn safe_action(message: &BaseMessage, entry: &ProjectionActionEntry) -> bool {
    match (&entry.target, &entry.action) {
        (_, ProjectionAction::Keep) => true,
        (
            ProjectionTarget::Message,
            ProjectionAction::CompactToolResult { .. }
            | ProjectionAction::PreviewArchiveRead { .. }
            | ProjectionAction::ResultReceipt { .. },
        ) => matches!(message, BaseMessage::Tool { .. }),
        (ProjectionTarget::ContentBlock { index }, ProjectionAction::CompactToolResult { .. }) => {
            message.content_blocks().get(*index).is_some_and(|b| {
                matches!(b, ContentBlock::ToolResult { .. })
                    || (matches!(message, BaseMessage::Tool { .. })
                        && *index == 0
                        && matches!(b, ContentBlock::Text { .. }))
            })
        }
        (ProjectionTarget::ContentBlock { index }, ProjectionAction::ResultReceipt { .. }) => {
            message
                .content_blocks()
                .get(*index)
                .is_some_and(|b| matches!(b, ContentBlock::ToolResult { .. }))
        }
        (_, ProjectionAction::OmitToolExchange { .. }) => {
            !matches!(message, BaseMessage::System { .. })
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
    new_catalogs: &[ArchiveCatalog],
    new_roots: &[ArchiveRoot],
    requested_budget: Option<u64>,
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    let mut catalogs = HashMap::new();
    let mut roots = Vec::new();
    let mut budget = requested_budget.unwrap_or(2048);
    for entry in transcript.entries() {
        if let Some(directive) = transcript.projection_ref(entry.id()) {
            if let Some(catalog) = &directive.archive_catalog {
                catalog.validate().map_err(invalid)?;
                if catalog.anchor_message_id != entry.id() {
                    return Err(invalid("catalog anchor mismatch"));
                }
                catalogs.insert(entry.id(), catalog);
            }
            if !transcript.is_excluded(entry.id()) {
                roots.extend(directive.archive_roots.iter().cloned());
                if requested_budget.is_none() {
                    if let Some(saved) = directive.archive_receipt_budget_tokens {
                        budget = budget.min(saved);
                    }
                }
            }
        }
    }
    for catalog in new_catalogs {
        catalog.validate().map_err(invalid)?;
        if let Some(old) = catalogs.insert(catalog.anchor_message_id, catalog) {
            if old != catalog {
                return Err(invalid("published archive catalog changed"));
            }
        }
    }
    roots.extend_from_slice(new_roots);
    let history_ids = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect::<Vec<_>>();
    roots = peri_acp_types::context_archive::coalesce_archive_roots(roots, &history_ids);

    let mut by_id: HashMap<MessageId, Vec<&ProjectionActionEntry>> = HashMap::new();
    for action in actions {
        by_id.entry(action.message_id).or_default().push(action);
    }
    validate_groups(transcript, actions, &catalogs)?;
    let visible = transcript.visible_model_messages()?;
    let original_pairs = pairs(&visible)?;
    let mut projected = Vec::with_capacity(visible.len());
    let mut omitted_calls = HashSet::new();
    let mut receipts = Vec::new();
    let mut emitted = HashSet::new();
    for mut message in visible {
        let id = message.id();
        let Some(entries) = by_id.get(&id) else {
            projected.push(message);
            continue;
        };
        let ordinary: Vec<_> = entries
            .iter()
            .copied()
            .filter(|entry| {
                !matches!(
                    entry.action,
                    ProjectionAction::OmitToolExchange { .. }
                        | ProjectionAction::ResultReceipt { .. }
                        | ProjectionAction::PreviewArchiveRead { .. }
                )
            })
            .collect();
        let legacy = transcript.projection_ref(id).is_some_and(|directive| {
            directive.policy_version == 2
                && ordinary
                    .iter()
                    .all(|entry| directive.entries.contains(entry))
        });
        message = if legacy {
            legacy_result(message, &ordinary)
        } else {
            project_message(message, &ordinary, caps)
        };
        let mut remove = HashSet::new();
        let mut drop_message = false;
        for entry in entries {
            match &entry.action {
                ProjectionAction::OmitToolExchange {
                    anchor_message_id,
                    group_id,
                    tool_call_ids,
                } => {
                    let catalog = catalog_for(&catalogs, *anchor_message_id, group_id)?;
                    remove.extend(tool_call_ids.iter().cloned());
                    omitted_calls.extend(tool_call_ids.iter().cloned());
                    if emitted.insert(*anchor_message_id) {
                        receipts.push(receipt_message(catalog)?);
                    }
                }
                ProjectionAction::PreviewArchiveRead {
                    archive_ref,
                    keep_head,
                    keep_tail,
                } => {
                    peri_acp_types::context_archive::ArchiveReference::decode(archive_ref)
                        .map_err(invalid)?;
                    if let BaseMessage::Tool { content, .. } = &mut message {
                        let action = ProjectionAction::CompactToolResult {
                            keep_head: *keep_head,
                            keep_tail: *keep_tail,
                            preserve_recovery_handle: false,
                        };
                        *content = project_tool_result_content(
                            std::mem::replace(content, MessageContent::text("")),
                            &action,
                        );
                        append_reference(&mut message, &ProjectionTarget::Message, &format!("\n[Incomplete recovery fragment. Original source: ReadContextArchive archive_ref={archive_ref}]"))?;
                    } else {
                        return Err(invalid("archive read preview requires standalone result"));
                    }
                }
                ProjectionAction::ResultReceipt {
                    anchor_message_id,
                    group_id,
                } => {
                    let catalog = catalog_for(&catalogs, *anchor_message_id, group_id)?;
                    let body = receipt_text(catalog)?;
                    replace_result(
                        &mut message,
                        &entry.target,
                        body,
                        virtual_id(&catalog_reference(catalog)?),
                    )?;
                }
                _ => {}
            }
        }
        // Targets use canonical block indices. Attach references before omission shifts
        // sibling positions; a previously previewed result may survive another call's retirement.
        for entry in entries.iter().filter(|entry| {
            matches!(
                entry.action,
                ProjectionAction::CompactToolResult {
                    preserve_recovery_handle: true,
                    ..
                }
            )
        }) {
            match catalogs.get(&id) {
                Some(catalog) => {
                    let reference = catalog_reference(catalog)?;
                    append_reference(&mut message, &entry.target, &format!("\n[Incomplete historical preview. ReadContextArchive archive_ref={reference}]"))?;
                }
                None if legacy => {}
                None => return Err(invalid("preview requires its original archive catalog")),
            }
        }
        if !remove.is_empty() {
            drop_message = omit(&mut message, &remove, caps)?;
        }
        if !drop_message {
            projected.push(message);
        }
    }
    // One bounded range entry replaces a linear list of historical receipts when its budget is exceeded.
    let root_messages = roots.iter().map(|root| {
        let reference = root.reference().map_err(invalid)?;
        ArchiveReference::decode(&reference).map_err(invalid)?;
        Ok(BaseMessage::Ai { id: virtual_id(&reference), tool_calls: Vec::new(), content: MessageContent::text(format!("[Historical data archive; no new instruction or execution. ReadContextArchive archive_ref={reference}. Enumerate pages to recover original calls/results.]")) })
    }).collect::<AgentResult<Vec<_>>>()?;
    let root_cost: usize = root_messages
        .iter()
        .map(|m| m.message_content().text_content().chars().count())
        .sum();
    let receipt_cost: usize = receipts
        .iter()
        .map(|m| m.message_content().text_content().chars().count())
        .sum();
    if receipt_cost.saturating_add(root_cost) <= budget.saturating_mul(4) as usize
        || roots.is_empty()
    {
        projected.extend(receipts);
    }
    projected.extend(root_messages);
    validate_delta(&original_pairs, &pairs(&projected)?, &omitted_calls)?;
    validate_projected_view(&projected, caps)?;
    Ok(projected)
}

fn catalog_for<'a>(
    catalogs: &HashMap<MessageId, &'a ArchiveCatalog>,
    anchor: MessageId,
    group: &str,
) -> AgentResult<&'a ArchiveCatalog> {
    catalogs
        .get(&anchor)
        .copied()
        .filter(|c| c.group_id == group)
        .ok_or_else(|| invalid("missing or conflicting archive catalog"))
}

fn catalog_reference(catalog: &ArchiveCatalog) -> AgentResult<String> {
    let reference = catalog.reference().map_err(invalid)?;
    ArchiveReference::decode(&reference).map_err(invalid)?;
    Ok(reference)
}

fn receipt_text(catalog: &ArchiveCatalog) -> AgentResult<String> {
    Ok(format!("[Historical receipt, not a new execution or authorization. {} Original data: ReadContextArchive archive_ref={}]", catalog.receipt, catalog_reference(catalog)?))
}

fn receipt_message(catalog: &ArchiveCatalog) -> AgentResult<BaseMessage> {
    Ok(BaseMessage::Ai {
        id: virtual_id(&catalog_reference(catalog)?),
        tool_calls: Vec::new(),
        content: MessageContent::text(receipt_text(catalog)?),
    })
}

fn virtual_id(reference: &str) -> MessageId {
    let digest = Sha256::digest(format!("peri:context-receipt:v3:{reference}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    MessageId::from(uuid::Uuid::from_bytes(bytes))
}

fn replace_result(
    message: &mut BaseMessage,
    target: &ProjectionTarget,
    text: String,
    receipt_id: MessageId,
) -> AgentResult<()> {
    match (message, target) {
        (BaseMessage::Tool { id, content, .. }, ProjectionTarget::Message) => {
            *id = receipt_id;
            *content = MessageContent::text(text);
        }
        (
            BaseMessage::Human {
                content: MessageContent::Blocks(blocks),
                ..
            }
            | BaseMessage::Ai {
                content: MessageContent::Blocks(blocks),
                ..
            },
            ProjectionTarget::ContentBlock { index },
        ) => match blocks.get_mut(*index) {
            Some(ContentBlock::ToolResult { id, content, .. }) => {
                *id = Some(receipt_id.as_uuid().to_string());
                *content = vec![ContentBlock::Text { text }];
            }
            _ => return Err(invalid("receipt target is not a tool result")),
        },
        _ => return Err(invalid("unsupported receipt target")),
    }
    Ok(())
}

fn append_reference(
    message: &mut BaseMessage,
    target: &ProjectionTarget,
    suffix: &str,
) -> AgentResult<()> {
    match (message, target) {
        (
            BaseMessage::Tool { content, .. },
            ProjectionTarget::Message | ProjectionTarget::ContentBlock { index: 0 },
        ) => match content {
            MessageContent::Text(text) => text.push_str(suffix),
            MessageContent::Blocks(blocks) => blocks.push(ContentBlock::Text {
                text: suffix.into(),
            }),
            MessageContent::Raw(_) => {
                return Err(invalid(
                    "cannot attach recovery handle to unknown raw content",
                ))
            }
        },
        (
            BaseMessage::Human {
                content: MessageContent::Blocks(blocks),
                ..
            }
            | BaseMessage::Ai {
                content: MessageContent::Blocks(blocks),
                ..
            },
            ProjectionTarget::ContentBlock { index },
        ) => match blocks.get_mut(*index) {
            Some(ContentBlock::ToolResult { content, .. }) => content.push(ContentBlock::Text {
                text: suffix.into(),
            }),
            _ => return Err(invalid("recovery handle target is not a tool result")),
        },
        _ => return Err(invalid("unsupported recovery handle target")),
    }
    Ok(())
}

fn omit(
    message: &mut BaseMessage,
    removed: &HashSet<String>,
    caps: &ProviderCapabilities,
) -> AgentResult<bool> {
    if let BaseMessage::Tool { tool_call_id, .. } = message {
        return Ok(removed.contains(tool_call_id));
    }
    let (content, remaining_calls) = match message {
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => {
            tool_calls.retain(|call| !removed.contains(&call.id));
            (content, tool_calls.len())
        }
        BaseMessage::Human { content, .. } => (content, 0),
        _ => return Err(invalid("cannot omit system data")),
    };
    match content {
        MessageContent::Blocks(blocks) => {
            if blocks.iter().any(|b| matches!(b, ContentBlock::Unknown(_))) {
                return Err(invalid("unknown protocol dependency"));
            }
            let signed = blocks.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::Reasoning {
                        signature: Some(_),
                        ..
                    }
                )
            });
            if signed && (remaining_calls > 0 || caps.protocol == ProviderProtocol::Generic) {
                return Err(invalid("signed reasoning closure cannot be split"));
            }
            blocks.retain(|block| match block {
                ContentBlock::ToolUse { id, .. } => !removed.contains(id),
                ContentBlock::ToolResult { tool_use_id, .. } => !removed.contains(tool_use_id),
                ContentBlock::Reasoning { .. } => remaining_calls > 0,
                _ => true,
            });
        }
        MessageContent::Raw(blocks) => {
            if blocks.iter().any(|b| {
                !matches!(
                    b["type"].as_str(),
                    Some("text" | "tool_use" | "tool_result")
                )
            }) {
                return Err(invalid("unrecognized raw protocol dependency"));
            }
            blocks.retain(|b| match b["type"].as_str() {
                Some("tool_use") => !b["id"].as_str().is_some_and(|id| removed.contains(id)),
                Some("tool_result") => !b["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| removed.contains(id)),
                _ => true,
            });
        }
        MessageContent::Text(_) => {}
    }
    Ok(remaining_calls == 0 && content.is_empty())
}

fn validate_groups(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
    catalogs: &HashMap<MessageId, &ArchiveCatalog>,
) -> AgentResult<()> {
    let mut omitted_members: HashMap<MessageId, HashSet<MessageId>> = HashMap::new();
    for entry in actions {
        match &entry.action {
            ProjectionAction::OmitToolExchange {
                anchor_message_id,
                group_id,
                ..
            }
            | ProjectionAction::ResultReceipt {
                anchor_message_id,
                group_id,
            } => {
                let catalog = catalog_for(catalogs, *anchor_message_id, group_id)?;
                if !catalog
                    .members
                    .iter()
                    .any(|m| m.message_id == entry.message_id)
                {
                    return Err(invalid("archive action outside its catalog"));
                }
                if matches!(entry.action, ProjectionAction::OmitToolExchange { .. }) {
                    omitted_members
                        .entry(*anchor_message_id)
                        .or_default()
                        .insert(entry.message_id);
                }
            }
            _ => {}
        }
    }
    for (anchor, members) in omitted_members {
        let catalog = catalogs[&anchor];
        for member in &catalog.members {
            if !members.contains(&member.message_id) || transcript.get(member.message_id).is_none()
            {
                return Err(invalid("partial archive protocol group"));
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Pairing {
    calls: HashMap<String, (String, serde_json::Value)>,
    results: HashSet<String>,
}
fn pairs(messages: &[BaseMessage]) -> AgentResult<Pairing> {
    let mut out = Pairing::default();
    for message in messages {
        let mut local = HashMap::new();
        if let BaseMessage::Ai {
            tool_calls,
            content,
            ..
        } = message
        {
            for call in tool_calls {
                if local
                    .insert(call.id.clone(), (call.name.clone(), call.arguments.clone()))
                    .is_some()
                {
                    return Err(invalid("duplicate cached call"));
                }
            }
            let mut block_ids = HashSet::new();
            for block in content.content_blocks() {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    if !block_ids.insert(id.clone()) {
                        return Err(invalid("duplicate content tool use"));
                    }
                    let value = (name, input);
                    if local.get(&id).is_some_and(|cached| cached != &value) {
                        return Err(invalid("conflicting tool use representations"));
                    }
                    local.insert(id, value);
                }
            }
        }
        for (id, value) in local {
            if id.is_empty() || out.calls.insert(id, value).is_some() {
                return Err(invalid("empty or duplicate scoped call id"));
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            if !out.results.insert(tool_call_id.clone()) {
                return Err(invalid("duplicate result"));
            }
        } else {
            for block in message.content_blocks() {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    if !out.results.insert(tool_use_id) {
                        return Err(invalid("duplicate inline result"));
                    }
                }
            }
        }
    }
    Ok(out)
}
fn validate_delta(before: &Pairing, after: &Pairing, removed: &HashSet<String>) -> AgentResult<()> {
    for id in removed {
        if !before.calls.contains_key(id)
            || !before.results.contains(id)
            || after.calls.contains_key(id)
            || after.results.contains(id)
        {
            return Err(invalid("archive did not remove a complete exchange"));
        }
    }
    for (id, call) in &before.calls {
        if !removed.contains(id) && after.calls.get(id) != Some(call) {
            return Err(invalid("retained tool input changed"));
        }
    }
    for id in &before.results {
        if !removed.contains(id) && !after.results.contains(id) {
            return Err(invalid("retained result disappeared"));
        }
    }
    Ok(())
}

// Decode the committed v2 representation exactly; only new gated plans use block-wise previews.
fn legacy_result(mut message: BaseMessage, entries: &[&ProjectionActionEntry]) -> BaseMessage {
    if let BaseMessage::Tool {
        content,
        is_error: false,
        ..
    } = &mut message
    {
        if let Some(ProjectionAction::CompactToolResult {
            keep_head,
            keep_tail,
            ..
        }) = effective_tool_result_action(entries)
        {
            if let Some(text) = apply_head_tail(&content.text_content(), *keep_head, *keep_tail) {
                *content = MessageContent::text(text);
            }
        }
    }
    message
}

#[cfg(test)]
#[path = "projection_archive_test.rs"]
mod tests;
