use super::*;
use peri_acp_types::{
    identity::AttemptId,
    session::TurnId,
    session_resources::{
        work::{
            ReasonRequest, WorkAdmission, WorkDecision, WorkGuard, WorkQuery, WorkSnapshot,
            WorkTarget,
        },
        ControlAttempt,
    },
};
use sha2::{Digest, Sha256};

fn large_publication() -> PreparedWorkCommand {
    let mut raw = command().into_command();
    if let WorkAction::PublishDelivery { delivery } = &mut raw.action {
        delivery.event.content = WorkPayload::from_payload(&PersistedPayload::Message(
            BaseMessage::human("请求正文\\\"雪".repeat(32 * 1024)),
        ))
        .unwrap();
    }
    PreparedWorkCommand::try_new(raw).unwrap()
}

#[test]
fn journal_and_mutation_share_command_and_raw_guard_buffers() {
    let command = large_publication();
    let cloned = command.clone();
    assert!(std::ptr::eq(command.command(), cloned.command()));
    assert!(Arc::ptr_eq(command.encoded(), cloned.encoded()));
    let journal = command_effects(&command).unwrap();
    let raw = noncanonical_state();
    let control = ControlState::default();
    let reduction = reduce_work(&command, &control, state(Some(&raw), false).unwrap()).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let effects = mutation_effects(&cloned, raw.clone(), None, &control, &reduction).unwrap();
    for parameter in [
        &journal[0].params[3],
        &journal[1].params[3],
        &effects[0].params[3],
        &effects[1].params[3],
    ] {
        assert!(Arc::ptr_eq(parameter, command.encoded()));
    }
    assert!(Arc::ptr_eq(&effects[2].params[1], &effects[3].params[1]));
    assert_eq!(effects[2].params[1].as_ref(), raw);
    assert_eq!(effects[0].sql, INSERT_COMMAND);
    assert_eq!(effects[1].sql, GUARD_COMMAND);
    assert_eq!(effects[2].sql, INSERT_STATE);
    assert_eq!(effects[3].sql, GUARD_STATE);
    let logical_command_bytes = [&journal[0], &journal[1], &effects[0], &effects[1]]
        .iter()
        .map(|effect| effect.params[3].len())
        .sum::<usize>();
    assert_eq!(logical_command_bytes, command.encoded().len() * 4);
    let unique_buffers: std::collections::HashSet<_> =
        [&journal[0], &journal[1], &effects[0], &effects[1]]
            .iter()
            .map(|effect| (effect.params[3].as_ptr(), effect.params[3].len()))
            .collect();
    assert_eq!(unique_buffers.len(), 1);
    assert_eq!(
        unique_buffers.iter().map(|(_, bytes)| bytes).sum::<usize>(),
        command.encoded().len()
    );
    for (insert, guard) in [(INSERT_EVENT, GUARD_EVENT)] {
        let insert = effects.iter().find(|effect| effect.sql == insert).unwrap();
        let guard = effects.iter().find(|effect| effect.sql == guard).unwrap();
        for (left, right) in insert.params.iter().zip(&guard.params) {
            assert!(Arc::ptr_eq(left, right));
        }
    }
}

#[test]
fn prepared_journal_wire_matches_original_and_cold_validation_is_canonical() {
    let command = large_publication();
    let canonical = serde_json::to_string(command.command()).unwrap();
    assert_eq!(command.encoded().as_ref(), canonical);
    assert_eq!(command.digest(), command.command().digest().unwrap());
    let raw = serde_json::to_string_pretty(command.command()).unwrap();
    let raw_digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
    assert_ne!(command.digest(), raw_digest);
    let owned = owned_command(&raw, command.digest(), None, false).unwrap();
    assert_eq!(owned.command, *command.command());
    assert!(owned.pending);
    assert!(owned_command(&raw, &raw_digest, None, false).is_err());
    assert!(owned_command(&raw, command.digest(), None, true).is_err());
    assert!(original_command("{}").is_err());
    let mut invalid = command.clone().into_command();
    invalid.session_id.clear();
    assert!(original_command(&serde_json::to_string(&invalid).unwrap()).is_err());
    let mut replacement = command.clone().into_command();
    replacement.mutation_id = "new-mutation".into();
    let replacement = PreparedWorkCommand::try_new(replacement).unwrap();
    assert_ne!(command.digest(), replacement.digest());
    assert_ne!(command, replacement);
}

fn transition_command(mutation_id: &str, action: WorkAction) -> PreparedWorkCommand {
    let mut raw = command().into_command();
    raw.mutation_id = mutation_id.into();
    raw.action = action;
    PreparedWorkCommand::try_new(raw).unwrap()
}

fn transition(
    command: &PreparedWorkCommand,
    control: ControlState,
    current: WorkState,
) -> (ControlState, WorkState) {
    let raw = serde_json::to_string_pretty(&current).unwrap();
    let reduction = reduce_work(command, &control, current).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let effects = mutation_effects(command, raw.clone(), None, &control, &reduction).unwrap();
    if matches!(command.action, WorkAction::ClaimBatch { .. }) {
        assert!(!reduction.projections.is_empty());
        let insert = effects
            .iter()
            .find(|effect| effect.sql == INSERT_PROJECTION)
            .unwrap();
        let guard = effects
            .iter()
            .find(|effect| effect.sql == GUARD_PROJECTION)
            .unwrap();
        assert!(insert.params[3].len() > 128 * 1024);
        for (left, right) in insert.params.iter().zip(&guard.params) {
            assert!(Arc::ptr_eq(left, right));
        }
    }
    assert_eq!(effects[0].sql, INSERT_COMMAND);
    assert_eq!(effects[1].sql, GUARD_COMMAND);
    assert_eq!(effects[2].sql, INSERT_STATE);
    assert_eq!(effects[3].sql, GUARD_STATE);
    assert!(Arc::ptr_eq(&effects[0].params[3], command.encoded()));
    assert!(Arc::ptr_eq(&effects[0].params[3], &effects[1].params[3]));
    assert!(Arc::ptr_eq(&effects[2].params[1], &effects[3].params[1]));
    assert_eq!(effects[3].params[1].as_ref(), raw);
    assert_eq!(effects.last().unwrap().sql, INSERT_RECEIPT);
    let persisted = effects
        .iter()
        .find(|effect| effect.sql == UPDATE_STATE)
        .unwrap();
    assert_eq!(
        persisted.params[1].as_ref(),
        encode(reduction.state.as_ref().unwrap()).unwrap()
    );
    (
        reduction.control.unwrap_or(control),
        reduction.state.unwrap(),
    )
}

#[test]
fn normal_reason_sequence_reuses_prepared_effects_with_large_request() {
    let (control, state) = transition(
        &large_publication(),
        ControlState::default(),
        WorkState::default(),
    );
    let query = WorkQuery {
        session_id: command().session_id.clone(),
        limit: 64,
    };
    let snapshot = WorkSnapshot::from_state(&query, control.clone(), state);
    let candidate = &snapshot.candidates[0];
    let work_id = candidate.work_id.clone();
    let register = transition_command(
        "register",
        WorkAction::RegisterAdmission {
            admission: WorkAdmission {
                session_id: query.session_id.clone(),
                admission_id: "admission".into(),
                instance_id: "instance".into(),
                generation_id: "generation".into(),
                lifecycle: control.lifecycle,
                control_generation: control.control_generation,
                work_id: work_id.clone(),
                work_revision: candidate.work_revision,
                execution: ControlAttempt {
                    turn_id: TurnId::new(),
                    attempt_id: AttemptId::new(),
                },
            },
        },
    );
    let delivery_ids = candidate.delivery_ids.clone();
    let (control, state) = transition(&register, control, snapshot.state);
    let guard = |control: &ControlState, state: &WorkState| WorkGuard {
        expected_revision: state.revision,
        expected_control_generation: control.control_generation,
        execution: control.attempt.clone().unwrap(),
    };
    let claim = transition_command(
        "claim",
        WorkAction::ClaimBatch {
            guard: guard(&control, &state),
            batch_id: work_id.clone(),
            delivery_ids,
        },
    );
    let (control, state) = transition(&claim, control, state);
    let serialized_request = serde_json::to_string(&"request 雪\\\"".repeat(32 * 1024)).unwrap();
    let request_digest = format!("{:x}", Sha256::digest(serialized_request.as_bytes()));
    let begin = transition_command(
        "begin",
        WorkAction::BeginReason {
            request_id: "request-id".into(),
            guard: guard(&control, &state),
            target: WorkTarget {
                work_id: work_id.clone(),
                expected_work_revision: state.works[&work_id].revision,
            },
            request: ReasonRequest {
                serialized_request: serialized_request.clone(),
                request_digest,
                model_ref: "model".into(),
                authorization_ref: "authorization".into(),
            },
        },
    );
    let (_, state) = transition(&begin, control, state);
    assert_eq!(
        state.works[&work_id]
            .reason_request
            .as_ref()
            .unwrap()
            .serialized_request,
        serialized_request
    );
}
