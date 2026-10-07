use super::*;

fn command(input_json: &str) -> WorkCommand {
    WorkCommand {
        session_id: "session".into(),
        recipient_lifecycle: 1,
        mutation_id: "mutation".into(),
        action: WorkAction::StageUserInput {
            input_json: input_json.into(),
            command_id: "command".into(),
            fingerprint: 7,
        },
    }
}

#[test]
fn prepared_command_preserves_exact_canonical_bytes_and_golden_digest() {
    let raw = command("{}");
    let original_bytes = serde_json::to_vec(&raw).unwrap();
    let original_digest = raw.digest().unwrap();
    let prepared = PreparedWorkCommand::try_new(raw).unwrap();
    assert_eq!(prepared.encoded().as_bytes(), original_bytes);
    assert_eq!(
        prepared.encoded().as_ref(),
        r#"{"sessionId":"session","recipientLifecycle":1,"mutationId":"mutation","action":{"kind":"stageUserInput","input_json":"{}","command_id":"command","fingerprint":7}}"#
    );
    assert_eq!(
        prepared.digest(),
        "57367a269e2f669ed848db221d64e736383cc697052fbb066f204008909725b3"
    );
    assert_eq!(prepared.digest(), original_digest);
}

#[test]
fn prepared_command_preserves_unicode_and_escaped_canonical_bytes() {
    let raw = command("中文\n\"quoted\"\\path\t\u{0000}🦀");
    let original_bytes = serde_json::to_vec(&raw).unwrap();
    let original_digest = raw.digest().unwrap();
    let prepared = PreparedWorkCommand::try_new(raw).unwrap();
    assert_eq!(prepared.encoded().as_bytes(), original_bytes);
    assert_eq!(prepared.digest(), original_digest);
}

#[test]
fn prepared_large_command_clone_and_reuse_share_one_immutable_preparation() {
    PREPARE_COUNTS.with(|counts| counts.set((0, 0)));
    let prepared = PreparedWorkCommand::try_new(command(&"x".repeat(1024 * 1024))).unwrap();
    assert_eq!(PREPARE_COUNTS.with(|counts| counts.get()), (1, 1));
    let encoded_pointer = prepared.encoded().as_ptr();
    let digest_pointer = prepared.digest().as_ptr();
    let payload_pointer = match &prepared.action {
        WorkAction::StageUserInput { input_json, .. } => input_json.as_ptr(),
        _ => unreachable!(),
    };
    for _ in 0..32 {
        let reused = prepared.clone();
        assert!(Arc::ptr_eq(&prepared.inner, &reused.inner));
        assert!(Arc::ptr_eq(prepared.encoded(), reused.encoded()));
        assert_eq!(reused.encoded().as_ptr(), encoded_pointer);
        assert_eq!(reused.digest().as_ptr(), digest_pointer);
        match &reused.action {
            WorkAction::StageUserInput { input_json, .. } => {
                assert_eq!(input_json.as_ptr(), payload_pointer);
            }
            _ => unreachable!(),
        }
        assert_eq!(reused.command(), prepared.command());
        assert_eq!(reused.digest(), prepared.digest());
        assert_eq!(PREPARE_COUNTS.with(|counts| counts.get()), (1, 1));
    }
}

#[test]
fn prepared_equality_uses_complete_command_not_identity_or_digest() {
    let prepared = PreparedWorkCommand::try_new(command("original")).unwrap();
    let independent = PreparedWorkCommand::try_new(command("original")).unwrap();
    assert_eq!(prepared, prepared.clone());
    assert_eq!(prepared, independent);
    let different = PreparedWorkCommand {
        inner: Arc::new(PreparedWorkCommandInner {
            command: command("changed"),
            encoded: prepared.encoded().clone(),
            digest: prepared.digest().into(),
        }),
    };
    assert_ne!(prepared, different);
}

#[test]
fn preparation_rejects_each_original_invalid_identity() {
    let mut invalid_session = command("{}");
    invalid_session.session_id.clear();
    let mut invalid_mutation = command("{}");
    invalid_mutation.mutation_id.clear();
    let mut oversized_mutation = command("{}");
    oversized_mutation.mutation_id = "x".repeat(257);
    let mut invalid_lifecycle = command("{}");
    invalid_lifecycle.recipient_lifecycle = 0;
    for raw in [
        invalid_session,
        invalid_mutation,
        oversized_mutation,
        invalid_lifecycle,
    ] {
        PREPARE_COUNTS.with(|counts| counts.set((0, 0)));
        let original_error = raw.digest().unwrap_err();
        let prepared_error = PreparedWorkCommand::try_new(raw).unwrap_err();
        assert_eq!(prepared_error.to_string(), original_error.to_string());
        assert_eq!(PREPARE_COUNTS.with(|counts| counts.get()), (0, 0));
    }
}

#[test]
fn into_command_moves_sole_owned_payload_and_preserves_shared_preparation() {
    let prepared = PreparedWorkCommand::try_new(command("large payload")).unwrap();
    let payload_pointer = match &prepared.action {
        WorkAction::StageUserInput { input_json, .. } => input_json.as_ptr(),
        _ => unreachable!(),
    };
    let moved = prepared.into_command();
    match &moved.action {
        WorkAction::StageUserInput { input_json, .. } => {
            assert_eq!(input_json.as_ptr(), payload_pointer);
        }
        _ => unreachable!(),
    }
    let prepared = PreparedWorkCommand::try_new(moved.clone()).unwrap();
    let shared = prepared.clone();
    assert_eq!(shared.into_command(), moved);
    assert_eq!(prepared.command(), &moved);
    assert_eq!(prepared.digest(), moved.digest().unwrap());
}
