//! Archive-aware model projection. Source messages remain immutable.
use super::render::{project_message_ref, RenderAction};
use super::*;
use crate::session::transcript::TranscriptEntry;
use peri_acp_types::context_archive::{ArchiveCatalog, ArchiveReference, ArchiveRoot};
use peri_acp_types::system_reminder::ReminderAudience;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    ToolResult,
    Other,
}

/// Borrowed classification of `content_blocks()[index]` without cloning the body.
///
/// Matches the shared `ContentBlock` deserializer: `Raw` values with missing
/// required fields deserialize as `Unknown`, not as their nominal type.
fn block_kind(content: &MessageContent, index: usize) -> Option<BlockKind> {
    match content {
        MessageContent::Text(text) => (index == 0 && !text.is_empty()).then_some(BlockKind::Text),
        MessageContent::Blocks(blocks) => blocks.get(index).map(|block| match block {
            ContentBlock::Text { .. } => BlockKind::Text,
            ContentBlock::ToolResult { .. } => BlockKind::ToolResult,
            _ => BlockKind::Other,
        }),
        MessageContent::Raw(values) => {
            values.get(index).map(|value| match value["type"].as_str() {
                Some("text") if raw_block_parses(value) => BlockKind::Text,
                Some("tool_result") if raw_block_parses(value) => BlockKind::ToolResult,
                _ => BlockKind::Other,
            })
        }
    }
}

/// Mirrors the shared `ContentBlock` deserializer's success conditions.
///
/// A raw block that fails deserialization becomes `ContentBlock::Unknown`; the
/// pairing index and restore safety checks must not treat it as its nominal type.
fn raw_block_parses(value: &serde_json::Value) -> bool {
    match value["type"].as_str() {
        Some("text") => value["text"].as_str().is_some(),
        Some("image") => value.get("source").is_some_and(raw_image_source_parses),
        Some("document") => value.get("source").is_some_and(raw_document_source_parses),
        Some("tool_use") => value["id"].as_str().is_some() && value["name"].as_str().is_some(),
        Some("tool_result") => {
            value["tool_use_id"].as_str().is_some()
                && value.get("content").is_none_or(|content| {
                    content
                        .as_array()
                        .is_some_and(|items| items.iter().all(raw_block_parses))
                })
        }
        Some("reasoning") => value["text"].as_str().is_some(),
        // `Unknown` accepts any JSON value, including non-objects.
        _ => true,
    }
}

fn raw_image_source_parses(source: &serde_json::Value) -> bool {
    match source["type"].as_str() {
        Some("base64") => {
            source["media_type"].as_str().is_some() && source["data"].as_str().is_some()
        }
        Some("url") => source["url"].as_str().is_some(),
        _ => false,
    }
}

fn raw_document_source_parses(source: &serde_json::Value) -> bool {
    match source["type"].as_str() {
        Some("base64") => {
            source["media_type"].as_str().is_some() && source["data"].as_str().is_some()
        }
        Some("url") => source["url"].as_str().is_some(),
        Some("text") => source["text"].as_str().is_some(),
        _ => false,
    }
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
            match block_kind(message.message_content(), *index) {
                Some(BlockKind::ToolResult) => true,
                Some(BlockKind::Text) => matches!(message, BaseMessage::Tool { .. }) && *index == 0,
                _ => false,
            }
        }
        (ProjectionTarget::ContentBlock { index }, ProjectionAction::ResultReceipt { .. }) => {
            matches!(
                block_kind(message.message_content(), *index),
                Some(BlockKind::ToolResult)
            )
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
    // Roots keep the canonical message that carries their directive. A root whose
    // `last_anchor` was removed by Full is rendered after that carrier, never at the
    // tail of the request.
    let mut root_holders: Vec<(ArchiveRoot, Option<MessageId>)> = Vec::new();
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
                for root in &directive.archive_roots {
                    root_holders.push((root.clone(), Some(entry.id())));
                }
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
    root_holders.extend(
        new_roots
            .iter()
            .map(|root| (root.clone(), Some(root.last_anchor))),
    );
    let history_ids = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect::<Vec<_>>();
    let roots = peri_acp_types::context_archive::coalesce_archive_roots(
        root_holders.iter().map(|(root, _)| root.clone()).collect(),
        &history_ids,
    );

    let mut by_id: HashMap<MessageId, Vec<&ProjectionActionEntry>> = HashMap::new();
    for action in actions {
        by_id.entry(action.message_id).or_default().push(action);
    }
    validate_groups(transcript, actions, &catalogs)?;
    // Borrowed view: normal entries keep the canonical body; only canonical and
    // legacy compact reminders allocate an encoded message.
    let visible = transcript
        .entries()
        .iter()
        .filter(|entry| !transcript.is_excluded(entry.id()))
        .filter(|entry| entry.visible_to(ReminderAudience::Model))
        .map(TranscriptEntry::project_message_ref)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut original_pairs = Pairing::default();
    for message in &visible {
        original_pairs.add_message(message.as_ref())?;
    }
    // Receipts and root entries are derived history: they keep the canonical step
    // order instead of becoming the newest messages in the request.
    let mut visible_positions = HashMap::with_capacity(visible.len());
    for (index, message) in visible.iter().enumerate() {
        visible_positions.insert(message.as_ref().id(), index);
    }
    let result_positions = visible_result_positions(&visible);
    let tail = visible.len();
    let mut outputs = Vec::with_capacity(visible.len());
    let mut omitted_calls = HashSet::new();
    let mut receipts: Vec<(usize, usize, BaseMessage)> = Vec::new();
    let mut emitted = HashSet::new();
    for borrowed in &visible {
        let message = borrowed.as_ref();
        let id = message.id();
        let Some(entries) = by_id.get(&id) else {
            outputs.push(Some(message.clone()));
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
        let mut remove = HashSet::new();
        let mut resolved = Vec::new();
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
                        let (insertion, anchor_position) = receipt_insertion_index(
                            transcript,
                            catalog,
                            *anchor_message_id,
                            &visible_positions,
                            &result_positions,
                            tail,
                        );
                        receipts.push((insertion, anchor_position, receipt_message(catalog)?));
                    }
                }
                ProjectionAction::PreviewArchiveRead {
                    archive_ref,
                    keep_head,
                    keep_tail,
                } => {
                    ArchiveReference::decode(archive_ref).map_err(invalid)?;
                    resolved.push(RenderAction::Preview {
                        action: ProjectionAction::CompactToolResult {
                            keep_head: *keep_head,
                            keep_tail: *keep_tail,
                            preserve_recovery_handle: false,
                        },
                        suffix: format!("\n[Incomplete recovery fragment. Original source: ReadContextArchive archive_ref={archive_ref}]"),
                    });
                }
                ProjectionAction::ResultReceipt {
                    anchor_message_id,
                    group_id,
                } => {
                    let catalog = catalog_for(&catalogs, *anchor_message_id, group_id)?;
                    resolved.push(RenderAction::Receipt {
                        target: entry.target.clone(),
                        text: receipt_text(catalog)?,
                        receipt_id: virtual_id(&catalog_reference(catalog)?),
                    });
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
                    resolved.push(RenderAction::Reference {
                        target: entry.target.clone(),
                        suffix: format!("\n[Incomplete historical preview. ReadContextArchive archive_ref={reference}]"),
                    });
                }
                None if legacy => {}
                None => return Err(invalid("preview requires its original archive catalog")),
            }
        }
        outputs.push(project_message_ref(
            message, &ordinary, legacy, &resolved, &remove, caps,
        )?);
    }
    // A root follows its visible `last_anchor`; when Full removed that member it follows
    // the message that still carries the root directive (usually the summary).
    let mut root_positions: HashMap<MessageId, usize> = HashMap::new();
    for (root, holder) in &root_holders {
        let position = visible_positions
            .get(&root.last_anchor)
            .or_else(|| holder.and_then(|id| visible_positions.get(&id)))
            .copied()
            .unwrap_or(tail);
        root_positions
            .entry(root.last_anchor)
            .and_modify(|current| *current = (*current).max(position))
            .or_insert(position);
    }
    let mut root_messages = roots
        .iter()
        .enumerate()
        .map(|(order, root)| {
            let reference = root.reference().map_err(invalid)?;
            ArchiveReference::decode(&reference).map_err(invalid)?;
            let position = root_positions
                .get(&root.last_anchor)
                .copied()
                .unwrap_or(tail);
            Ok((
                position,
                order,
                BaseMessage::Ai { id: virtual_id(&reference), tool_calls: Vec::new(), content: MessageContent::text(format!("[Historical data archive; no new instruction or execution. ReadContextArchive archive_ref={reference}. Enumerate pages to recover original calls/results.]")) },
            ))
        })
        .collect::<AgentResult<Vec<_>>>()?;
    let root_cost: usize = root_messages
        .iter()
        .map(|(_, _, message)| message.message_content().text_content().chars().count())
        .sum();
    let receipt_cost: usize = receipts
        .iter()
        .map(|(_, _, message)| message.message_content().text_content().chars().count())
        .sum();
    let keep_receipts = receipt_cost.saturating_add(root_cost) <= budget.saturating_mul(4) as usize
        || roots.is_empty();
    // insertion position -> derived messages, appended after the canonical message at
    // that position (even when that message itself is fully omitted).
    let mut derived: Vec<Vec<BaseMessage>> = (0..=tail).map(|_| Vec::new()).collect();
    if keep_receipts {
        receipts.sort_by_key(|(insertion, anchor_position, _)| (*insertion, *anchor_position));
        for (insertion, _, message) in receipts {
            derived[insertion.min(tail)].push(message);
        }
    }
    root_messages.sort_by_key(|(position, order, _)| (*position, *order));
    for (position, _, message) in root_messages {
        derived[position.min(tail)].push(message);
    }
    let extra_len: usize = derived.iter().map(Vec::len).sum();
    let mut projected = Vec::with_capacity(visible.len() + extra_len);
    for (index, output) in outputs.into_iter().enumerate() {
        if let Some(message) = output {
            projected.push(message);
        }
        projected.append(&mut derived[index]);
    }
    projected.append(&mut derived[tail]);
    let after_pairs = Pairing::from_messages(&projected)?;
    validate_delta(&original_pairs, &after_pairs, &omitted_calls)?;
    validate_projected_view(&projected, caps)?;
    Ok(projected)
}

/// Visible message index for every tool call result in the current view.
fn visible_result_positions(
    visible: &[std::borrow::Cow<'_, BaseMessage>],
) -> HashMap<String, usize> {
    let mut positions = HashMap::new();
    for (index, message) in visible.iter().enumerate() {
        let message = message.as_ref();
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            positions.insert(tool_call_id.clone(), index);
            continue;
        }
        match message.message_content() {
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                        positions.insert(tool_use_id.clone(), index);
                    }
                }
            }
            MessageContent::Raw(values) => {
                for value in values {
                    // Malformed raw results deserialize as Unknown and are not results.
                    if value["type"].as_str() == Some("tool_result") && raw_block_parses(value) {
                        if let Some(id) = value["tool_use_id"].as_str() {
                            positions.insert(id.to_string(), index);
                        }
                    }
                }
            }
            MessageContent::Text(_) => {}
        }
    }
    positions
}

/// Canonical boundary for a catalog receipt.
///
/// The receipt follows the whole step: every call declared by the anchor message and
/// its sibling callers, plus the last result position of those calls. This keeps A/B
/// batches intact when only A is archived and never splits a call from its result.
/// Returns `(insertion index, anchor order)` in visible-message coordinates.
fn receipt_insertion_index(
    transcript: &MessageTranscript,
    catalog: &ArchiveCatalog,
    anchor: MessageId,
    visible_positions: &HashMap<MessageId, usize>,
    result_positions: &HashMap<String, usize>,
    tail: usize,
) -> (usize, usize) {
    let anchor_position = visible_positions.get(&anchor).copied();
    let mut end = anchor_position;
    let mut calls: HashSet<&str> = HashSet::new();
    for member in &catalog.members {
        let member_position = visible_positions.get(&member.message_id).copied();
        end = match (end, member_position) {
            (Some(current), Some(position)) => Some(current.max(position)),
            (None, position) => position,
            (current, None) => current,
        };
        let Some(message) = transcript
            .get(member.message_id)
            .and_then(|entry| entry.as_message())
        else {
            continue;
        };
        calls.extend(message.tool_calls().iter().map(|call| call.id.as_str()));
        match message.message_content() {
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        calls.insert(id.as_str());
                    }
                }
            }
            MessageContent::Raw(values) => {
                for value in values {
                    if value["type"].as_str() == Some("tool_use") {
                        if let Some(id) = value["id"].as_str() {
                            calls.insert(id);
                        }
                    }
                }
            }
            MessageContent::Text(_) => {}
        }
    }
    for call in calls {
        if let Some(position) = result_positions.get(call) {
            end = Some(end.map_or(*position, |current| current.max(*position)));
        }
    }
    (end.unwrap_or(tail), anchor_position.unwrap_or(tail))
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

type CallRepr<'a> = (&'a str, &'a serde_json::Value);

/// Tool pairing over borrowed messages: no body or block clone.
#[derive(Default)]
struct Pairing<'a> {
    calls: HashMap<&'a str, CallRepr<'a>>,
    results: HashSet<&'a str>,
}

impl<'a> Pairing<'a> {
    fn from_messages(messages: &'a [BaseMessage]) -> AgentResult<Self> {
        let mut pairing = Self::default();
        for message in messages {
            pairing.add_message(message)?;
        }
        Ok(pairing)
    }

    fn add_message(&mut self, message: &'a BaseMessage) -> AgentResult<()> {
        let mut local: HashMap<&'a str, CallRepr<'a>> = HashMap::new();
        if let BaseMessage::Ai {
            tool_calls,
            content,
            ..
        } = message
        {
            for call in tool_calls {
                if local
                    .insert(call.id.as_str(), (call.name.as_str(), &call.arguments))
                    .is_some()
                {
                    return Err(invalid("duplicate cached call"));
                }
            }
            match content {
                MessageContent::Text(_) => {}
                MessageContent::Blocks(blocks) => {
                    let mut block_ids = HashSet::new();
                    for block in blocks {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            if !block_ids.insert(id.as_str()) {
                                return Err(invalid("duplicate content tool use"));
                            }
                            let value = (name.as_str(), input);
                            if local
                                .get(id.as_str())
                                .is_some_and(|cached| *cached != value)
                            {
                                return Err(invalid("conflicting tool use representations"));
                            }
                            local.insert(id.as_str(), value);
                        }
                    }
                }
                MessageContent::Raw(values) => {
                    let mut block_ids = HashSet::new();
                    for value in values {
                        if value["type"].as_str() != Some("tool_use") {
                            continue;
                        }
                        // Missing required fields deserialize as Unknown, whose tool
                        // data is intentionally not trusted by the pairing index.
                        let (Some(id), Some(name)) = (value["id"].as_str(), value["name"].as_str())
                        else {
                            continue;
                        };
                        if !block_ids.insert(id) {
                            return Err(invalid("duplicate content tool use"));
                        }
                        let input: &'a serde_json::Value = match value.get("input") {
                            Some(input) => input,
                            None => empty_json_object(),
                        };
                        let value = (name, input);
                        if local.get(id).is_some_and(|cached| *cached != value) {
                            return Err(invalid("conflicting tool use representations"));
                        }
                        local.insert(id, value);
                    }
                }
            }
        }
        for (id, value) in local {
            if id.is_empty() || self.calls.insert(id, value).is_some() {
                return Err(invalid("empty or duplicate scoped call id"));
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            if !self.results.insert(tool_call_id.as_str()) {
                return Err(invalid("duplicate result"));
            }
        } else {
            match message.message_content() {
                MessageContent::Text(_) => {}
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            if !self.results.insert(tool_use_id.as_str()) {
                                return Err(invalid("duplicate inline result"));
                            }
                        }
                    }
                }
                MessageContent::Raw(values) => {
                    for value in values {
                        // Malformed raw results deserialize as Unknown and are not
                        // part of the pairing index.
                        if value["type"].as_str() != Some("tool_result") || !raw_block_parses(value)
                        {
                            continue;
                        }
                        if let Some(tool_use_id) = value["tool_use_id"].as_str() {
                            if !self.results.insert(tool_use_id) {
                                return Err(invalid("duplicate inline result"));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn empty_json_object() -> &'static serde_json::Value {
    static EMPTY: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| serde_json::Value::Object(Default::default()))
}

fn validate_delta(
    before: &Pairing<'_>,
    after: &Pairing<'_>,
    removed: &HashSet<String>,
) -> AgentResult<()> {
    for id in removed {
        if !before.calls.contains_key(id.as_str())
            || !before.results.contains(id.as_str())
            || after.calls.contains_key(id.as_str())
            || after.results.contains(id.as_str())
        {
            return Err(invalid("archive did not remove a complete exchange"));
        }
    }
    for (id, call) in &before.calls {
        if !removed.contains(*id) && after.calls.get(*id) != Some(call) {
            return Err(invalid("retained tool input changed"));
        }
    }
    for id in &before.results {
        if !removed.contains(*id) && !after.results.contains(*id) {
            return Err(invalid("retained result disappeared"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "projection_archive_test.rs"]
mod tests;
