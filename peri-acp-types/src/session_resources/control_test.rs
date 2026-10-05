use super::*;

#[test]
fn control_wire_is_camel_case_and_requires_exact_stop_target() {
    let target = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let command = ControlCommand {
        session_id: "session".into(),
        command_id: "stable".into(),
        expected_lifecycle: 1,
        expected_revision: 0,
        expected_control_generation: 0,
        action: ControlAction::Stop {
            target: target.clone(),
        },
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["commandId"], "stable");
    assert_eq!(
        json["action"]["target"]["turnId"],
        serde_json::to_value(target.turn_id).unwrap()
    );
    assert_eq!(
        serde_json::from_value::<ControlCommand>(json.clone()).unwrap(),
        command
    );
    let mut missing = json;
    missing["action"]["target"]
        .as_object_mut()
        .unwrap()
        .remove("turnId");
    assert!(serde_json::from_value::<ControlCommand>(missing).is_err());
}

#[test]
fn control_internal_actions_cannot_be_deserialized_from_wire() {
    let mut command = ControlCommand {
        session_id: "session".into(),
        command_id: "stable".into(),
        expected_lifecycle: 1,
        expected_revision: 0,
        expected_control_generation: 0,
        action: ControlAction::FinishClose,
    };
    assert!(
        serde_json::from_value::<ControlCommand>(serde_json::to_value(&command).unwrap()).is_err()
    );
    command.action = ControlAction::ObserveAttempt { target: None };
    assert!(
        serde_json::from_value::<ControlCommand>(serde_json::to_value(&command).unwrap()).is_err()
    );
}

#[test]
fn control_digest_covers_identity_versions_action_and_exact_target() {
    let original = ControlCommand {
        session_id: "session".into(),
        command_id: "stable".into(),
        expected_lifecycle: 1,
        expected_revision: 0,
        expected_control_generation: 0,
        action: ControlAction::Stop {
            target: ControlAttempt {
                turn_id: TurnId::new(),
                attempt_id: AttemptId::new(),
            },
        },
    };
    let digest = original.digest().unwrap();
    assert_eq!(original.digest().unwrap(), digest);
    let mut changed = original.clone();
    changed.expected_control_generation = 1;
    assert_ne!(changed.digest().unwrap(), digest);
    changed = original.clone();
    changed.action = ControlAction::Pause;
    assert_ne!(changed.digest().unwrap(), digest);
    changed = original.clone();
    if let ControlAction::Stop { target } = &mut changed.action {
        target.turn_id = TurnId::new();
    }
    assert_ne!(changed.digest().unwrap(), digest);
}
