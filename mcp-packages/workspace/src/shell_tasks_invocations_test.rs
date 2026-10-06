use super::*;
use std::sync::Arc;

async fn settled(owner: &ShellTasks, scope: &str) {
    peri_time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let changed = owner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if owner.snapshot(scope).resources_settled {
                return;
            }
            changed.await;
        }
    })
    .await
    .expect("actual owned resource callbacks must prove settlement");
}

fn metadata(
    owner: &ShellTasks,
    scope: &str,
    invocation: &str,
    input: &serde_json::Value,
) -> RequestMetaObject {
    let capabilities = owner.owner_capabilities(scope);
    let serialized = serde_json::to_string(input).unwrap();
    let mut metadata = RequestMetaObject::new();
    metadata.0.0.insert("peri.invocation".into(), serde_json::json!({
        "version": 1, "invocationId": invocation, "scopeId": scope,
        "initiatorSessionId": scope, "recipientLifecycle": 1,
        "ownerIdentity": capabilities["ownerIdentity"], "scopeEpoch": capabilities["scopeEpoch"],
        "authorizationRef": "trusted-policy", "argumentsJson": serialized,
        "argumentsDigest": format!("{:x}", Sha256::digest(serialized.as_bytes())),
        "toolName": "Bash"
    }));
    metadata
}

#[tokio::test]
async fn accepted_invocation_retains_real_task_binding_without_rpc_response_ack() {
    let owner = ShellTasks::new();
    let input = serde_json::json!({"command":"sleep 30","run_in_background":true});
    let meta = metadata(&owner, "child", "invocation-1", &input);
    assert_eq!(
        owner.accept_invocation("child", &meta, &input).unwrap(),
        Some("invocation-1".into())
    );
    let directory = tempfile::tempdir().unwrap();
    let task = owner
        .spawn_scoped_for_invocation(
            "sleep 30".into(),
            directory.path().to_string_lossy().into_owned(),
            None,
            Some("child"),
            Some("invocation-1"),
        )
        .await
        .unwrap();
    let discovered = owner.discover_invocations("child");
    assert_eq!(discovered["invocations"][0]["ownerTaskId"], task.task_id);
    assert_eq!(discovered["invocations"][0]["responsePrepared"], false);
    assert!(owner.accept_invocation("child", &meta, &input).is_err());
    assert_eq!(owner.snapshot("child").tasks.len(), 1);
    owner.close_scope("child", 0).await.unwrap();
    owner.cancel_scoped(&task.task_id, "child").unwrap();
    settled(&owner, "child").await;
    assert!(owner.snapshot("child").resources_settled);
}

#[tokio::test]
async fn interrupted_owned_creation_keeps_invocation_discoverable_after_request_drop() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut owner = ShellTasks::new();
    owner.spawn_gate = Some(gate.clone());
    let input = serde_json::json!({"command":"sleep 30"});
    let meta = metadata(&owner, "child", "invocation-drop", &input);
    owner.accept_invocation("child", &meta, &input).unwrap();
    let worker_owner = owner.clone();
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().to_string_lossy().into_owned();
    let request = tokio::spawn(async move {
        worker_owner
            .spawn_scoped_for_invocation(
                "sleep 30".into(),
                cwd,
                None,
                Some("child"),
                Some("invocation-drop"),
            )
            .await
    });
    for _ in 0..100 {
        if owner
            .state
            .lock()
            .inflight
            .get("child")
            .copied()
            .unwrap_or_default()
            > 0
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        owner
            .state
            .lock()
            .inflight
            .get("child")
            .copied()
            .unwrap_or_default()
            > 0
    );
    request.abort();
    let _ = request.await;
    gate.notify_one();
    peri_time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if owner.discover_invocations("child")["invocations"][0]["ownerTaskId"].is_string() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    owner.close_scope("child", 0).await.unwrap();
    let task_id = owner.discover_invocations("child")["invocations"][0]["ownerTaskId"]
        .as_str()
        .unwrap()
        .to_owned();
    owner.cancel_scoped(&task_id, "child").unwrap();
    settled(&owner, "child").await;
    assert!(owner.snapshot("child").resources_settled);
}

#[test]
fn foreign_identity_epoch_or_arguments_are_rejected_before_owner_admission() {
    let owner = ShellTasks::new();
    let input = serde_json::json!({"command":"echo safe"});
    let original = metadata(&owner, "child", "invocation-1", &input);
    for (field, invalid) in [
        ("scopeId", serde_json::json!("root")),
        ("ownerIdentity", serde_json::json!("new-owner")),
        ("scopeEpoch", serde_json::json!(999)),
    ] {
        let mut meta = original.clone();
        meta.0 .0.get_mut("peri.invocation").unwrap()[field] = invalid;
        assert!(owner.accept_invocation("child", &meta, &input).is_err());
    }
    assert!(owner
        .accept_invocation(
            "child",
            &original,
            &serde_json::json!({"command":"echo other"})
        )
        .is_err());
    assert_eq!(
        owner.discover_invocations("child")["invocations"],
        serde_json::json!([])
    );
}
