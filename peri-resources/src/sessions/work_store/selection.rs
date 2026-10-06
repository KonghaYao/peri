use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{
    ControlState, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use serde::de::DeserializeOwned;

use super::{SqlParam, SqlRows, SqlStatement, payload::integer};
use crate::sessions::{failure::corrupt, work::decode};

const READ_HEAD: &str = "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 UNION ALL SELECT 1 FROM session_control_state WHERE session_id=?1),c.state_json,h.record_json,EXISTS(WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT 1 FROM session_work_commands WHERE session_id IN(SELECT id FROM scope) AND kind='mutation' AND reconciled=0),EXISTS(SELECT 1 FROM session_processing WHERE session_id=?1 AND phase='blocked') FROM (SELECT 1) LEFT JOIN session_control_state c ON c.session_id=?1 LEFT JOIN session_work_head h ON h.session_id=?1 AND h.lifecycle=COALESCE(json_extract(c.state_json,'$.lifecycle'),1)";
const READ_AUX: &str = "SELECT c.kind,CAST(p.bytes AS TEXT),c.subject_id FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id WHERE c.session_id=?1 AND (?2 IS NULL OR c.lifecycle=?2) AND c.kind=?3 AND (?4 IS NULL OR c.subject_id=?4) AND c.subject_id>?5 ORDER BY c.subject_id LIMIT ?6";

fn text(value: impl Into<String>) -> SqlParam {
    SqlParam::Text(value.into())
}

fn base(session: &str) -> SqlStatement {
    SqlStatement::new(READ_HEAD, vec![text(session)])
}

fn bounded(sql: &'static str, query: &WorkQuery, extra: Vec<SqlParam>) -> SqlStatement {
    let mut params = vec![
        text(&query.session_id),
        text(query.cursor.as_deref().unwrap_or("")),
        SqlParam::Integer(i64::from(query.limit)),
    ];
    params.extend(extra);
    SqlStatement::new(sql, params)
}

fn auxiliary(
    query: &WorkQuery,
    kind: &str,
    subject: Option<&str>,
    lifecycle: Option<u64>,
) -> SessionResourceResult<SqlStatement> {
    Ok(SqlStatement::new(
        READ_AUX,
        vec![
            text(&query.session_id),
            lifecycle
                .map(integer)
                .transpose()?
                .unwrap_or(SqlParam::Null),
            text(kind),
            subject.map(text).unwrap_or(SqlParam::Null),
            text(query.cursor.as_deref().unwrap_or("")),
            SqlParam::Integer(query.limit.into()),
        ],
    ))
}

pub(crate) fn inspection_plan(query: &WorkQuery) -> SessionResourceResult<Vec<SqlStatement>> {
    query.validate()?;
    let mut statements = vec![base(&query.session_id)];
    let statement = match &query.selector {
        WorkSelector::Head => return Ok(statements),
        WorkSelector::Availability => {
            statements.push(bounded("SELECT 'availabilityProcessing',p.record_json,p.processing_id FROM session_processing p WHERE p.session_id=?1 AND p.phase NOT IN ('settled','abandoned') AND p.processing_id>?2 ORDER BY p.processing_id LIMIT ?3",query,vec![]));
            statements.push(bounded("SELECT 'availabilityDelivery',delivery_id,delivery_id FROM session_deliveries WHERE session_id=?1 AND lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1) AND status='pending' AND delivery_id>?2 ORDER BY sequence,delivery_id LIMIT MIN(?3,COALESCE((SELECT json_extract(record_json,'$.limits.maxBatchSize') FROM session_work_head WHERE session_id=?1 AND lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1)),64))",query,vec![]));
            return Ok(statements);
        }
        WorkSelector::Inbox => bounded(
            "SELECT 'delivery',d.record_json,printf('%020d:%s',d.sequence,d.delivery_id) FROM session_deliveries d WHERE d.session_id=?1 AND d.lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1) AND d.status='pending' AND (d.sequence,d.delivery_id)>(CAST(substr(?2,1,20) AS INTEGER),substr(?2,22)) ORDER BY d.sequence,d.delivery_id LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::Delivery { delivery_id } => bounded(
            "SELECT 'delivery',record_json,delivery_id FROM session_deliveries WHERE session_id=?1 AND delivery_id=?4 AND delivery_id>?2 LIMIT ?3",
            query,
            vec![text(delivery_id)],
        ),
        WorkSelector::Processing { processing_id } => bounded(
            "SELECT 'processing',record_json,processing_id FROM session_processing WHERE session_id=?1 AND processing_id=?4 AND processing_id>?2 LIMIT ?3",
            query,
            vec![text(processing_id)],
        ),
        WorkSelector::CurrentProcessing => bounded(
            "SELECT 'processing',p.record_json,p.processing_id FROM session_processing p JOIN session_work_head h ON h.session_id=p.session_id AND h.lifecycle=p.lifecycle WHERE p.session_id=?1 AND p.processing_id=json_extract(h.record_json,'$.currentProcessingId') AND p.processing_id>?2 ORDER BY p.processing_id LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::ActiveProcessing => bounded(
            "SELECT 'processing',record_json,processing_id FROM session_processing WHERE session_id=?1 AND phase NOT IN ('settled','abandoned') AND processing_id>?2 ORDER BY processing_id LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::ProcessingDeliveries { processing_id } => bounded(
            "SELECT 'delivery',record_json,printf('%020d:%s',batch_ordinal,delivery_id) FROM session_deliveries WHERE session_id=?1 AND processing_id=?4 AND (batch_ordinal,delivery_id)>(CAST(substr(?2,1,20) AS INTEGER),substr(?2,22)) ORDER BY batch_ordinal,delivery_id LIMIT ?3",
            query,
            vec![text(processing_id)],
        ),
        WorkSelector::Effects {
            processing_id,
            phase_sequence,
        } => bounded(
            "SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND processing_id=?4 AND (?5 IS NULL OR phase_sequence=?5) AND invocation_id>?2 ORDER BY invocation_id LIMIT ?3",
            query,
            vec![
                text(processing_id),
                phase_sequence
                    .map(integer)
                    .transpose()?
                    .unwrap_or(SqlParam::Null),
            ],
        ),
        WorkSelector::EffectsById { invocation_ids } => {
            if invocation_ids.len() > query.limit as usize {
                return Err(corrupt("effect selection exceeds requested bound"));
            }
            for invocation_id in invocation_ids {
                statements.push(bounded("SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND invocation_id=?4 AND invocation_id>?2 LIMIT ?3", query, vec![text(invocation_id)]));
            }
            return Ok(statements);
        }
        WorkSelector::Effect { invocation_id } => bounded(
            "SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND invocation_id=?4 AND invocation_id>?2 LIMIT ?3",
            query,
            vec![text(invocation_id)],
        ),
        WorkSelector::TaskBinding {
            owner_identity,
            owner_task_id,
        } => bounded(
            "SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND owner_identity=?4 AND owner_task_id=?5 AND invocation_id>?2 LIMIT ?3",
            query,
            vec![text(owner_identity), text(owner_task_id)],
        ),
        WorkSelector::TaskBindingByTask { owner_task_id } => bounded(
            "SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND owner_task_id=?4 AND invocation_id>?2 ORDER BY invocation_id LIMIT MIN(?3,2)",
            query,
            vec![text(owner_task_id)],
        ),
        WorkSelector::Drafts => bounded(
            "SELECT 'draft',record_json,printf('%020d:%s',fifo_seq,input_id) FROM session_inputs WHERE session_id=?1 AND lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1) AND status='queued' AND (fifo_seq,input_id)>(CAST(substr(?2,1,20) AS INTEGER),substr(?2,22)) ORDER BY fifo_seq,input_id LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::UnresolvedInputs => bounded(
            "SELECT 'draft',draft.record_json,printf('%020d:%s',draft.fifo_seq,draft.input_id) FROM session_inputs draft WHERE draft.session_id=?1 AND draft.lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1) AND draft.status IN ('queued','published') AND (draft.status='queued' OR EXISTS(SELECT 1 FROM session_deliveries delivery WHERE delivery.session_id=draft.session_id AND delivery.lifecycle=draft.lifecycle AND delivery.delivery_id=json_extract(draft.record_json,'$.publicationId') AND json_extract(delivery.record_json,'$.projection') IS NULL AND json_extract(delivery.record_json,'$.disposition') IS NULL)) AND (draft.fifo_seq,draft.input_id)>(CAST(substr(?2,1,20) AS INTEGER),substr(?2,22)) ORDER BY draft.fifo_seq,draft.input_id LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::Draft { input_id } => bounded(
            "SELECT 'draft',record_json,input_id FROM session_inputs WHERE session_id=?1 AND lifecycle=COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1) AND input_id=?4 AND input_id>?2 LIMIT ?3",
            query,
            vec![text(input_id)],
        ),
        WorkSelector::CurrentAdmission => bounded(
            "SELECT c.kind,CAST(p.bytes AS TEXT),c.subject_id FROM session_work_commands c JOIN session_work_head h ON h.session_id=c.session_id AND h.lifecycle=c.lifecycle JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id WHERE c.session_id=?1 AND c.kind='admission' AND c.subject_id=json_extract(h.record_json,'$.currentAdmissionId') AND c.subject_id>?2 LIMIT ?3",
            query,
            vec![],
        ),
        WorkSelector::Admission { admission_id } => {
            auxiliary(query, "admission", Some(admission_id), None)?
        }
        WorkSelector::Delegation {
            parent_session_id,
            parent_lifecycle,
            delegation_id,
        } => bounded(
            "SELECT 'processing',record_json,processing_id FROM session_processing WHERE session_id=?1 AND parent_session=?4 AND delegation_id=?5 AND json_extract(record_json,'$.delegation.parentLifecycle')=?6 AND processing_id>?2 ORDER BY processing_id LIMIT ?3",
            query,
            vec![
                text(parent_session_id),
                text(delegation_id),
                integer(*parent_lifecycle)?,
            ],
        ),
        WorkSelector::RecoveryDescriptor { lifecycle } => {
            auxiliary(query, "recoveryDescriptor", None, Some(*lifecycle))?
        }
        WorkSelector::TerminalCommands => auxiliary(query, "terminal", None, None)?,
        WorkSelector::TerminalCommand { admission_id } => {
            auxiliary(query, "terminal", Some(admission_id), None)?
        }
        WorkSelector::LegacyEvidence { record_id } => {
            auxiliary(query, "legacy", Some(record_id), None)?
        }
        WorkSelector::Command { mutation_id } => bounded(
            "SELECT 'command',CAST(p.bytes AS TEXT),c.mutation_id,c.digest,r.resolution_json,c.reconciled FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id LEFT JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.session_id=?1 AND c.mutation_id=?4 AND c.kind='mutation' AND c.mutation_id>?2 LIMIT ?3",
            query,
            vec![text(mutation_id)],
        ),
        WorkSelector::PendingCommands => bounded(
            "SELECT 'command',CAST(p.bytes AS TEXT),c.mutation_id,c.digest,r.resolution_json,c.reconciled FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id LEFT JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.session_id=?1 AND c.reconciled=0 AND c.kind='mutation' AND c.mutation_id>?2 ORDER BY c.mutation_id LIMIT ?3",
            query,
            vec![],
        ),
    };
    statements.push(statement);
    Ok(statements)
}

pub(crate) fn head(rows: &[SqlRows]) -> SessionResourceResult<(ControlState, SessionWorkHead)> {
    let row = rows
        .first()
        .and_then(|rows| rows.first())
        .ok_or_else(|| corrupt("missing work head result"))?;
    let [
        SqlParam::Integer(exists),
        control,
        head,
        SqlParam::Integer(_),
        SqlParam::Integer(_),
    ] = row.as_slice()
    else {
        return Err(corrupt("invalid work head row"));
    };
    if *exists == 0 {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::NotFound,
        ));
    }
    let control = match control {
        SqlParam::Null => ControlState::default(),
        SqlParam::Text(json) => decode(json)?,
        _ => return Err(corrupt("invalid control row")),
    };
    let head: SessionWorkHead = match head {
        SqlParam::Null => SessionWorkHead {
            lifecycle: control.lifecycle,
            ..SessionWorkHead::default()
        },
        SqlParam::Text(json) => decode(json)?,
        _ => return Err(corrupt("invalid head row")),
    };
    if head.lifecycle != control.lifecycle {
        return Err(corrupt("head lifecycle conflicts"));
    }
    Ok((control, head))
}

fn records<Record: DeserializeOwned>(rows: &[SqlRows]) -> SessionResourceResult<Vec<Record>> {
    rows.iter()
        .skip(1)
        .flatten()
        .map(|row| match row.get(1) {
            Some(SqlParam::Text(json)) => decode(json),
            _ => Err(corrupt("invalid selected work record")),
        })
        .collect()
}

pub(crate) fn decode_inspection(
    query: &WorkQuery,
    rows: &[SqlRows],
) -> SessionResourceResult<WorkInspection> {
    query.validate()?;
    let (control, head) = head(rows)?;
    let page = match &query.selector {
        WorkSelector::Head => WorkPage::Head,
        WorkSelector::Availability => {
            let first = rows
                .first()
                .and_then(|rows| rows.first())
                .ok_or_else(|| corrupt("missing availability row"))?;
            let pending = matches!(first.get(3),Some(SqlParam::Integer(value)) if *value!=0);
            let blocked = head.legacy_unknown > 0
                || matches!(first.get(4),Some(SqlParam::Integer(value)) if *value!=0);
            let mut candidates = Vec::new();
            let mut delivery_ids = Vec::new();
            for row in rows.iter().skip(1).flatten() {
                let (Some(SqlParam::Text(kind)), Some(SqlParam::Text(json))) =
                    (row.first(), row.get(1))
                else {
                    return Err(corrupt("invalid availability selection"));
                };
                if kind == "availabilityProcessing" {
                    let processing: Processing = decode(json)?;
                    candidates.push(WorkCandidate {
                        work_id: processing.processing_id,
                        work_revision: processing.revision,
                        stage: processing.stage,
                        requires_recovery: matches!(
                            processing.stage,
                            WorkStage::ReasonInFlight | WorkStage::Blocked
                        ),
                        delivery_ids: vec![],
                    });
                } else if kind == "availabilityDelivery" {
                    delivery_ids.push(json.clone());
                } else {
                    return Err(corrupt("invalid availability kind"));
                }
            }
            if !delivery_ids.is_empty() && candidates.len() < query.limit as usize {
                candidates.push(WorkCandidate {
                    work_id: format!(
                        "batch:{}:{}:{}",
                        query.session_id, head.lifecycle, head.next_delivery_seq
                    ),
                    work_revision: 0,
                    stage: WorkStage::ReasonReady,
                    requires_recovery: false,
                    delivery_ids,
                });
            }
            if blocked || pending {
                candidates.clear();
            }
            WorkPage::Availability(WorkAvailability {
                lifecycle: head.lifecycle,
                change_seq: head.change_seq,
                blocked,
                pending,
                candidates,
            })
        }
        WorkSelector::Inbox
        | WorkSelector::Delivery { .. }
        | WorkSelector::ProcessingDeliveries { .. } => WorkPage::Deliveries(records(rows)?),
        WorkSelector::Processing { .. }
        | WorkSelector::CurrentProcessing
        | WorkSelector::ActiveProcessing
        | WorkSelector::Delegation { .. } => WorkPage::Processings(records(rows)?),
        WorkSelector::Effects { .. }
        | WorkSelector::EffectsById { .. }
        | WorkSelector::Effect { .. }
        | WorkSelector::TaskBinding { .. }
        | WorkSelector::TaskBindingByTask { .. } => WorkPage::Effects(records(rows)?),
        WorkSelector::Drafts | WorkSelector::UnresolvedInputs | WorkSelector::Draft { .. } => {
            WorkPage::Drafts(records(rows)?)
        }
        WorkSelector::CurrentAdmission | WorkSelector::Admission { .. } => {
            WorkPage::Admissions(records(rows)?)
        }
        WorkSelector::RecoveryDescriptor { .. } => WorkPage::RecoveryDescriptors(records(rows)?),
        WorkSelector::TerminalCommands | WorkSelector::TerminalCommand { .. } => {
            WorkPage::TerminalCommands(records(rows)?)
        }
        WorkSelector::LegacyEvidence { .. } => WorkPage::LegacyEvidence(records(rows)?),
        WorkSelector::Command { .. } | WorkSelector::PendingCommands => {
            let commands = rows
                .iter()
                .skip(1)
                .flatten()
                .map(|row| {
                    let [
                        SqlParam::Text(_),
                        SqlParam::Text(json),
                        SqlParam::Text(_),
                        SqlParam::Text(digest),
                        resolution,
                        SqlParam::Integer(reconciled),
                    ] = row.as_slice()
                    else {
                        return Err(corrupt("invalid command row"));
                    };
                    let resolution = match resolution {
                        SqlParam::Null => None,
                        SqlParam::Text(json) => Some(json.as_str()),
                        _ => return Err(corrupt("invalid receipt row")),
                    };
                    crate::sessions::work::owned_command(json, digest, resolution, *reconciled != 0)
                })
                .collect::<SessionResourceResult<Vec<_>>>()?;
            WorkPage::Commands(commands)
        }
    };
    let selected: Vec<_> = rows.iter().skip(1).flatten().collect();
    if !matches!(query.selector, WorkSelector::Availability)
        && selected.len() > query.limit as usize
    {
        return Err(corrupt("selected page exceeds requested bound"));
    }
    let next_cursor = if !matches!(query.selector, WorkSelector::Availability)
        && selected.len() == query.limit as usize
    {
        selected.last().and_then(|row| match row.get(2) {
            Some(SqlParam::Text(cursor)) => Some(cursor.clone()),
            _ => None,
        })
    } else {
        None
    };
    Ok(WorkInspection {
        session_id: query.session_id.clone(),
        control,
        head,
        page,
        next_cursor,
    })
}

pub(crate) fn read_set(command: &WorkCommand) -> SessionResourceResult<Vec<SqlStatement>> {
    command.digest()?;
    let mut selectors = Vec::new();
    match &command.action {
        WorkAction::StageUserInput { input_id, .. }
        | WorkAction::WithdrawStagedUserInput { input_id, .. } => {
            selectors.push(WorkSelector::Draft {
                input_id: input_id.clone(),
            })
        }
        WorkAction::PublishStagedUserInputs { deliveries, .. } => {
            selectors.push(WorkSelector::Drafts);
            selectors.push(WorkSelector::CurrentProcessing);
            for delivery in deliveries {
                selectors.push(WorkSelector::Delivery {
                    delivery_id: delivery.delivery_id.clone(),
                });
            }
        }
        WorkAction::PublishDelivery { delivery } => selectors.push(WorkSelector::Delivery {
            delivery_id: delivery.delivery_id.clone(),
        }),
        WorkAction::PublishTaskSettlement { delivery, binding } => {
            selectors.push(WorkSelector::Delivery {
                delivery_id: delivery.delivery_id.clone(),
            });
            selectors.push(WorkSelector::Effect {
                invocation_id: binding.invocation_id.clone(),
            });
        }
        WorkAction::ClaimBatch {
            batch_id,
            delivery_ids,
            ..
        } => {
            selectors.push(WorkSelector::Processing {
                processing_id: batch_id.clone(),
            });
            selectors.push(WorkSelector::CurrentAdmission);
            for delivery_id in delivery_ids {
                selectors.push(WorkSelector::Delivery {
                    delivery_id: delivery_id.clone(),
                });
            }
        }
        WorkAction::PrepareInvocation { intent, .. } => selectors.push(WorkSelector::Effect {
            invocation_id: intent.invocation_id.clone(),
        }),
        WorkAction::BeginDispatch {
            target,
            invocation_id,
            ..
        }
        | WorkAction::OutcomeUnknown {
            target,
            invocation_id,
            ..
        } => {
            selectors.push(WorkSelector::Processing {
                processing_id: target.work_id.clone(),
            });
            selectors.push(WorkSelector::Effect {
                invocation_id: invocation_id.clone(),
            });
        }
        WorkAction::CommitAct {
            target, results, ..
        } => {
            selectors.push(WorkSelector::Processing {
                processing_id: target.work_id.clone(),
            });
            for result in results {
                selectors.push(WorkSelector::Effect {
                    invocation_id: result.invocation_id.clone(),
                });
            }
        }
        WorkAction::CommitReasonResponseAndDispatchIntent {
            target,
            dispatch_intents,
            ..
        } => {
            selectors.push(WorkSelector::Processing {
                processing_id: target.work_id.clone(),
            });
            selectors.push(WorkSelector::ProcessingDeliveries {
                processing_id: target.work_id.clone(),
            });
            for intent in dispatch_intents {
                selectors.push(WorkSelector::Effect {
                    invocation_id: intent.invocation_id.clone(),
                });
            }
        }
        WorkAction::BeginReason { target, .. }
        | WorkAction::BlockWork { target, .. }
        | WorkAction::ResumeWork { target, .. }
        | WorkAction::AbandonWork { target, .. }
        | WorkAction::SettleWork { target, .. } => {
            selectors.push(WorkSelector::Processing {
                processing_id: target.work_id.clone(),
            });
            selectors.push(WorkSelector::ProcessingDeliveries {
                processing_id: target.work_id.clone(),
            });
            selectors.push(WorkSelector::Effects {
                processing_id: target.work_id.clone(),
                phase_sequence: None,
            });
        }
        WorkAction::RegisterAdmission { admission }
        | WorkAction::FinishAdmission { admission, .. } => {
            selectors.push(WorkSelector::Admission {
                admission_id: admission.admission_id.clone(),
            })
        }
        WorkAction::BindResourceOwners { .. } | WorkAction::BindChildResumeMetadata { .. } => {
            selectors.push(WorkSelector::RecoveryDescriptor {
                lifecycle: command.recipient_lifecycle,
            })
        }
        WorkAction::BindTerminalObligation { admission_id, .. }
        | WorkAction::AcknowledgeTerminalObligation { admission_id, .. } => {
            selectors.push(WorkSelector::TerminalCommand {
                admission_id: admission_id.clone(),
            })
        }
        WorkAction::BindWorkDelegation {
            work_id, binding, ..
        } => {
            selectors.push(WorkSelector::Processing {
                processing_id: work_id.clone(),
            });
            selectors.push(WorkSelector::Effect {
                invocation_id: binding.invocation_id.clone(),
            });
        }
        WorkAction::ReconcileTaskBinding { binding, .. } => {
            selectors.push(WorkSelector::Effect {
                invocation_id: binding.invocation_id.clone(),
            });
            selectors.push(WorkSelector::TaskBinding {
                owner_identity: binding.owner_identity.clone(),
                owner_task_id: binding.owner_task_id.clone(),
            });
        }
        WorkAction::WithdrawDelivery { delivery_id, .. }
        | WorkAction::AbandonDelivery { delivery_id, .. } => {
            selectors.push(WorkSelector::Delivery {
                delivery_id: delivery_id.clone(),
            })
        }
        WorkAction::ResetBudget { budget_id, .. } => selectors.push(WorkSelector::Processing {
            processing_id: budget_id.clone(),
        }),
        WorkAction::QuarantineLegacy { record_id, .. } => {
            selectors.push(WorkSelector::LegacyEvidence {
                record_id: record_id.clone(),
            })
        }
    }
    if selectors.len() > MAX_WORK_PAGE_SIZE as usize + 4 {
        return Err(corrupt("command selection exceeds bound"));
    }
    let mut statements = vec![base(&command.session_id)];
    for selector in selectors {
        let current_effects = matches!(
            selector,
            WorkSelector::Effects {
                phase_sequence: None,
                ..
            }
        );
        let mut selection = inspection_plan(&WorkQuery::new(&command.session_id, selector))?;
        for statement in selection.iter_mut().skip(1) {
            if current_effects {
                statement.sql = "SELECT 'effect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND processing_id=?4 AND phase_sequence=(SELECT phase_sequence FROM session_processing WHERE session_id=?1 AND processing_id=?4) AND (?5 IS NULL OR phase_sequence=?5) AND invocation_id>?2 ORDER BY invocation_id LIMIT ?3";
            }
            if statement.sql == READ_AUX {
                statement.params[5] = SqlParam::Integer(i64::from(MAX_WORK_PAGE_SIZE) + 1);
            } else if let Some(SqlParam::Integer(limit)) = statement.params.get_mut(2) {
                *limit = i64::from(MAX_WORK_PAGE_SIZE) + 1;
            }
        }
        statements.extend(selection.into_iter().skip(1));
    }
    if let WorkAction::BindWorkDelegation {
        parent_binding_receipt,
        ..
    } = &command.action
    {
        statements.push(SqlStatement::new("SELECT 'parentReceipt',resolution_json,mutation_id FROM session_work_receipts WHERE mutation_id=?1 AND session_id=?2",vec![text(&parent_binding_receipt.mutation_id),text(&parent_binding_receipt.session_id)]));
    }
    if let WorkAction::WithdrawDelivery { delivery_id, .. } = &command.action {
        statements.push(SqlStatement::new("SELECT 'draft',record_json,input_id FROM session_inputs WHERE session_id=?1 AND lifecycle=?2 AND json_extract(record_json,'$.publicationId')=?3 LIMIT 2",vec![text(&command.session_id),integer(command.recipient_lifecycle)?,text(delivery_id)]));
    }
    if let WorkAction::StageUserInput { input_id, .. } = &command.action {
        statements.push(SqlStatement::new("SELECT 'delivery',delivery.record_json,delivery.delivery_id FROM session_inputs draft JOIN session_deliveries delivery ON delivery.delivery_id=json_extract(draft.record_json,'$.publicationId') AND delivery.session_id=draft.session_id AND delivery.lifecycle=draft.lifecycle WHERE draft.session_id=?1 AND draft.lifecycle=?2 AND draft.input_id=?3 LIMIT 1",vec![text(&command.session_id),integer(command.recipient_lifecycle)?,text(input_id)]));
    }
    if let WorkAction::BindWorkDelegation { binding, .. } = &command.action {
        statements.push(SqlStatement::new("SELECT 'parentEffect',record_json,invocation_id FROM session_effects WHERE session_id=?1 AND invocation_id=?2 LIMIT 1",vec![text(&binding.initiator_session_id),text(&binding.invocation_id)]));
    }
    if let WorkAction::AcknowledgeTerminalObligation { receipt, .. } = &command.action {
        statements.push(SqlStatement::new("SELECT 'parentReceipt',resolution_json,mutation_id FROM session_work_receipts WHERE session_id=?1 AND mutation_id=?2 LIMIT 1",vec![text(&receipt.session_id),text(&receipt.mutation_id)]));
    }
    Ok(statements)
}

pub(crate) fn decode_facts(
    command: &WorkCommand,
    rows: &[SqlRows],
) -> SessionResourceResult<WorkFacts> {
    let (control, head) = head(rows)?;
    let mut facts = WorkFacts {
        session_id: command.session_id.clone(),
        control,
        head,
        processing: None,
        deliveries: vec![],
        effects: vec![],
        drafts: vec![],
        admission: None,
        recovery_descriptor: None,
        terminal_obligation: None,
        legacy_evidence: None,
        parent_binding_receipt: None,
        parent_effect: None,
    };
    for row in rows.iter().skip(1).flatten() {
        let (Some(SqlParam::Text(kind)), Some(SqlParam::Text(json))) = (row.first(), row.get(1))
        else {
            return Err(corrupt("invalid work facts row"));
        };
        match kind.as_str() {
            "delivery" => {
                let record: Delivery = decode(json)?;
                if !facts
                    .deliveries
                    .iter()
                    .any(|current| current.delivery_id == record.delivery_id)
                {
                    facts.deliveries.push(record);
                }
            }
            "effect" => {
                let record: Effect = decode(json)?;
                if !facts
                    .effects
                    .iter()
                    .any(|current| current.invocation_id == record.invocation_id)
                {
                    facts.effects.push(record);
                }
            }
            "draft" => facts.drafts.push(decode(json)?),
            "processing" => {
                if facts.processing.is_some() {
                    return Err(corrupt("multiple processing facts"));
                }
                facts.processing = Some(decode(json)?);
            }
            "admission" => facts.admission = Some(decode(json)?),
            "recoveryDescriptor" => facts.recovery_descriptor = Some(decode(json)?),
            "terminal" => facts.terminal_obligation = Some(decode(json)?),
            "legacy" => facts.legacy_evidence = Some(decode(json)?),
            "parentEffect" => facts.parent_effect = Some(decode(json)?),
            "parentReceipt" => {
                if let WorkResolution::Applied { receipt } = decode(json)? {
                    facts.parent_binding_receipt = Some(receipt);
                }
            }
            _ => return Err(corrupt("unknown work facts kind")),
        }
    }
    if facts.deliveries.len() > MAX_WORK_PAGE_SIZE as usize
        || facts.effects.len() > MAX_WORK_PAGE_SIZE as usize
        || facts.drafts.len() > MAX_WORK_PAGE_SIZE as usize
    {
        return Err(corrupt("work facts exceed bound"));
    }
    Ok(facts)
}
