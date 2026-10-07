use super::*;
use crate::host::execution_admission::ReverseExecutionAdmission;
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, AttemptStoppedProof, ExecutionAdmissionPort,
    SettlementOutcome, SettlementReceipt, SettlementRequest,
};
use peri_acp_types::execution_admission::{EntryOutcome, EntryRequest};
use peri_acp_types::session_resources::{
    work::{WorkCandidate, WorkSnapshot, WorkStage, WorkState},
    ControlState,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct JoinedDomainFixture {
    dispatcher: std::sync::OnceLock<std::sync::Weak<JsonlSdkDispatcher>>,
    stopped: AtomicBool,
    settled_reply: std::sync::OnceLock<Value>,
    fail_after_confirmation: bool,
}

struct PendingCommandDomainFixture {
    original: peri_acp_types::session_resources::work::WorkCommand,
    resolution: String,
    resolved: AtomicBool,
}

#[async_trait]
impl RequestTransport for PendingCommandDomainFixture {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        match method {
            "session/work/query" => Ok(json!({
                "control":ControlState::default(), "work":Value::Null,
                "pendingCommands":if self.resolved.load(Ordering::SeqCst) {
                    Vec::new()
                } else { vec![self.original.clone()] },
            })),
            "session/work/resolve" => {
                assert_eq!(
                    params,
                    json!({"sessionId":self.original.session_id,"command":self.original})
                );
                self.resolved
                    .store(self.resolution != "unknown", Ordering::SeqCst);
                Ok(json!({"status":self.resolution,"receipt":Value::Null}))
            }
            _ => panic!("unresolved domain mutation cannot enter execution: {method}"),
        }
    }
}

#[tokio::test]
async fn real_sdk_jsonl_resolves_only_original_domain_command_with_status_fixture() {
    use peri_acp_types::session_resources::work::{WorkAction, WorkCommand};
    for status in ["applied", "notApplied", "unknown"] {
        let directory = tempfile::tempdir().unwrap();
        let reverse = Arc::new(PendingCommandDomainFixture {
            original: WorkCommand {
                session_id: "pending-original-session".into(),
                recipient_lifecycle: 1,
                mutation_id: "original-stable-mutation".into(),
                action: WorkAction::BindResourceOwners {
                    expected_revision: 23,
                    connections_json: if status == "applied" {
                        "\n".repeat(8 * 1024 * 1024)
                    } else {
                        "{}".into()
                    },
                    authorization_ref: "original-owner-proof".into(),
                },
            },
            resolution: status.into(),
            resolved: AtomicBool::new(false),
        });
        let dispatcher = JsonlSdkDispatcher::launch_with_reverse_transport(
            launcher(directory.path().join("registry.db"), status),
            Some(reverse.clone()),
        )
        .await
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            dispatcher.send_request(
                "peri/execution/activate",
                json!({"sessionId":"pending-original-session"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            result["status"],
            if status == "unknown" {
                "unknown"
            } else {
                "idle"
            }
        );
        assert_eq!(reverse.resolved.load(Ordering::SeqCst), status != "unknown");
        let record = dispatcher
            .send_request(
                "peri/execution/query",
                json!({"sessionId":"pending-original-session"}),
            )
            .await
            .unwrap();
        assert!(record["attempt"].is_null());
        assert!(record["budgets"].as_array().unwrap().is_empty());
    }
}

#[async_trait]
impl RequestTransport for JoinedDomainFixture {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        match method {
            "session/work/query" => Ok(json!({
                "control":ControlState::default(),
                "work":if self.stopped.load(Ordering::SeqCst) { Value::Null } else {
                    json!({"workId":"fixture-work","revision":0,"lifecycle":1,"controlGeneration":0})
                },
            })),
            "session/execute" => {
                let admission = serde_json::from_value(params["ticket"].clone()).unwrap();
                let dispatcher = self.dispatcher.get().unwrap().upgrade().unwrap();
                let port = ReverseExecutionAdmission::new(dispatcher);
                let mut request = snapshot_request("confirm");
                request.existing_admission = Some(admission);
                request.request_id = request
                    .existing_admission
                    .as_ref()
                    .unwrap()
                    .admission_id
                    .clone();
                let AdmissionOutcome::Admitted { admission } = port.admit(request).await.unwrap()
                else {
                    panic!("fixture existing ticket must be confirmed by the SDK");
                };
                if self.fail_after_confirmation {
                    return Err(AcpError::new(
                        -32603,
                        "fixture execution entry outcome unknown",
                    ));
                }
                assert!(matches!(
                    port.entered(EntryRequest {
                        admission: admission.clone(),
                        entry_evidence_id: "joined-domain-entry-fixture".into(),
                    })
                    .await
                    .unwrap(),
                    EntryOutcome::Applied { .. }
                ));
                tokio::spawn(async { tokio::task::yield_now().await })
                    .await
                    .unwrap();
                let reply = json!({"status":"settled","ticket":admission,"proof":{
                    "kind":"attemptStopped","instanceId":admission.instance_id,"generationId":admission.generation_id,
                    "execution":admission.execution,"evidenceId":"joined-domain-fixture",
                }});
                self.settled_reply.set(reply.clone()).unwrap();
                self.stopped.store(true, Ordering::SeqCst);
                Ok(reply)
            }
            "session/execute/resolve" => {
                let Some(reply) = self.settled_reply.get() else {
                    return Ok(json!({"status":"unknown"}));
                };
                assert_eq!(params["ticket"], reply["ticket"]);
                Ok(json!({"status":"applied","reply":reply}))
            }
            _ => Err(AcpError::new(-32601, "fixture method unsupported")),
        }
    }
}

#[tokio::test]
async fn real_sdk_sqlite_entry_ack_marks_running_without_second_admission_with_receipt_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(launcher(directory.path().join("registry.db"), "entered"))
            .await
            .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let AdmissionOutcome::Admitted { admission } =
        port.admit(snapshot_request("entry-fixture")).await.unwrap()
    else {
        panic!("fixture needs a real SDK slot");
    };
    let request = EntryRequest {
        admission: admission.clone(),
        entry_evidence_id: "durable-store-entry-receipt-fixture".into(),
    };
    let first = port.entered(request.clone()).await.unwrap();
    assert!(matches!(first, EntryOutcome::Applied { .. }));
    assert_eq!(port.entered(request).await.unwrap(), first);
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(record["attempt"]["phase"], "running");
    assert_eq!(record["budgets"][0]["attempts"], 1);
}

#[tokio::test]
async fn real_sdk_sqlite_full_duplex_activation_confirms_existing_slot_with_joined_domain_fixture()
{
    let directory = tempfile::tempdir().unwrap();
    let reverse = Arc::new(JoinedDomainFixture {
        dispatcher: std::sync::OnceLock::new(),
        stopped: AtomicBool::new(false),
        settled_reply: std::sync::OnceLock::new(),
        fail_after_confirmation: false,
    });
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch_with_reverse_transport(
            launcher(directory.path().join("registry.db"), "activate"),
            Some(reverse.clone()),
        )
        .await
        .unwrap(),
    );
    reverse.dispatcher.set(Arc::downgrade(&dispatcher)).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dispatcher.send_request(
            "peri/execution/activate",
            json!({
                "sessionId":"sqlite-transport-session","source":"notification",
            }),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["status"], "idle");
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(record["budgets"][0]["attempts"], 1);
    assert_eq!(record["attempt"]["phase"], "settled");
    assert_eq!(
        record["attempt"]["stoppedProof"]["evidenceId"],
        "joined-domain-fixture"
    );
}

#[tokio::test]
async fn real_sdk_sqlite_unknown_execution_keeps_slot_and_blocks_duplicate_with_domain_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let reverse = Arc::new(JoinedDomainFixture {
        dispatcher: std::sync::OnceLock::new(),
        stopped: AtomicBool::new(false),
        settled_reply: std::sync::OnceLock::new(),
        fail_after_confirmation: true,
    });
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch_with_reverse_transport(
            launcher(directory.path().join("registry.db"), "unknown"),
            Some(reverse.clone()),
        )
        .await
        .unwrap(),
    );
    reverse.dispatcher.set(Arc::downgrade(&dispatcher)).unwrap();
    let result = dispatcher
        .send_request(
            "peri/execution/activate",
            json!({"sessionId":"sqlite-transport-session","source":"notification"}),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "unknown");
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(record["attempt"]["phase"], "entering");
    assert!(record["attempt"].get("stoppedProof").is_none());
    let port = ReverseExecutionAdmission::new(dispatcher);
    assert_eq!(
        port.admit(snapshot_request("no-duplicate-after-unknown"))
            .await
            .unwrap(),
        AdmissionOutcome::Busy
    );
}

fn launcher(database: PathBuf, generation: &str) -> SdkDispatcherLaunch {
    let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(if cfg!(windows) { "bun.exe" } else { "bun" }))
        .find(|candidate| candidate.is_file())
        .expect("real SDK JSONL gate requires Bun");
    let module = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("npm-packages/@peri-sdk/src/execution/sidecar.ts")
        .canonicalize()
        .expect("real SDK JSONL gate requires the production sidecar module");
    SdkDispatcherLaunch {
        executable,
        module,
        database,
        instance_id: format!("sqlite-test-{generation}"),
        generation_id: generation.into(),
    }
}

fn snapshot_request(request_id: &str) -> AdmissionRequest {
    AdmissionRequest {
        existing_admission: None,
        request_id: request_id.into(),
        snapshot: WorkSnapshot {
            pending_commands: Vec::new(),
            session_id: "sqlite-transport-session".into(),
            control: ControlState::default(),
            state: WorkState::default(),
            blocked: false,
            candidates: vec![WorkCandidate {
                work_id: "fixture-work".into(),
                work_revision: 0,
                stage: WorkStage::ReasonReady,
                batch_id: None,
                delivery_ids: Vec::new(),
                requires_recovery: false,
            }],
        }
        .into(),
    }
}

#[tokio::test]
async fn real_sqlite_large_required_payload_is_admitted_by_real_sdk_through_lean_snapshot() {
    use peri_acp_types::session::{MessageQueue, MessageSource, QueuedMessage};
    use peri_acp_types::session_resources::work::WorkQuery;
    use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
    let directory = tempfile::tempdir().unwrap();
    let resources = peri_agent::resources::open_session_resources_with(Some(
        directory.path().join("threads.db"),
    ))
    .await
    .unwrap();
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    let session_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: session_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new("{}"),
        })
        .await
        .unwrap();
    let queue = MessageQueue::default();
    queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        peri_acp_types::messages::BaseMessage::human("x".repeat(6 * 1024 * 1024)),
    ));
    peri_agent::agent::stages::publish_session_inbox(resources.clone(), &session_id, 1, &queue)
        .await
        .unwrap();
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(serde_json::to_vec(&snapshot).unwrap().len() > 4 * 1024 * 1024);
    assert_eq!(snapshot.candidates.len(), 1);
    let request = AdmissionRequest {
        request_id: "large-required-store-fixture".into(),
        snapshot: (&snapshot).into(),
        existing_admission: None,
    };
    let wire = serde_json::to_value(&request).unwrap();
    assert!(wire["snapshot"].get("state").is_none());
    assert!(wire["snapshot"].get("pendingCommands").is_none());
    assert!(serde_json::to_vec(&request).unwrap().len() < 4096);
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(launcher(
            directory.path().join("registry.db"),
            "large-required",
        ))
        .await
        .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let AdmissionOutcome::Admitted { admission } = port.admit(request).await.unwrap() else {
        panic!("durable large Required input must reach actual SDK admission")
    };
    snapshot.validate_admission(&admission).unwrap();
    let record = dispatcher
        .send_request("peri/execution/query", json!({"sessionId":session_id}))
        .await
        .unwrap();
    assert_eq!(
        record["attempt"]["ticket"],
        serde_json::to_value(admission).unwrap()
    );
    assert_eq!(record["attempt"]["phase"], "entering");
    assert_eq!(record["budgets"][0]["attempts"], 1);
}

#[tokio::test]
async fn real_sdk_sqlite_admission_is_unique_and_replay_stable_with_snapshot_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("registry.db");
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(launcher(database.clone(), "first"))
            .await
            .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let (first, second) = tokio::join!(
        port.admit(snapshot_request("request-first")),
        port.admit(snapshot_request("request-second"))
    );
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, AdmissionOutcome::Admitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, AdmissionOutcome::Busy))
            .count(),
        1
    );
    let (admitted_index, original) = outcomes
        .iter()
        .enumerate()
        .find_map(|(index, result)| match result {
            AdmissionOutcome::Admitted { admission } => Some((index, admission.clone())),
            _ => None,
        })
        .unwrap();
    let request_id = if admitted_index == 0 {
        "request-first"
    } else {
        "request-second"
    };
    assert_eq!(
        port.admit(snapshot_request(request_id)).await.unwrap(),
        AdmissionOutcome::Admitted {
            admission: original
        }
    );
    assert!(database.is_file());
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(record["attempt"]["phase"], "entering");
    assert_eq!(record["budgets"][0]["attempts"], 1);
}

#[tokio::test]
async fn real_sdk_sqlite_restart_cannot_claim_unknown_old_instance_with_snapshot_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("registry.db");
    {
        let dispatcher = Arc::new(
            JsonlSdkDispatcher::launch(launcher(database.clone(), "old"))
                .await
                .unwrap(),
        );
        let port = ReverseExecutionAdmission::new(dispatcher);
        assert!(matches!(
            port.admit(snapshot_request("before-restart"))
                .await
                .unwrap(),
            AdmissionOutcome::Admitted { .. }
        ));
    }
    let replacement = Arc::new(
        JsonlSdkDispatcher::launch(launcher(database, "replacement"))
            .await
            .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(replacement);
    assert!(matches!(
        port.admit(snapshot_request("after-restart")).await.unwrap(),
        AdmissionOutcome::Blocked { .. }
    ));
}

#[tokio::test]
async fn real_sdk_sqlite_existing_confirmation_rejects_forged_ticket_with_snapshot_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(launcher(directory.path().join("registry.db"), "confirm"))
            .await
            .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let AdmissionOutcome::Admitted { admission } = port
        .admit(snapshot_request("new-reservation"))
        .await
        .unwrap()
    else {
        panic!("real SDK must reserve a slot for the snapshot fixture");
    };
    let mut confirm = snapshot_request(&admission.admission_id);
    let before = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    confirm.existing_admission = Some(admission.clone());
    assert_eq!(
        port.admit(confirm.clone()).await.unwrap(),
        AdmissionOutcome::Admitted {
            admission: admission.clone()
        }
    );
    let mut forged = admission.clone();
    forged.admission_id = "forged-admission".into();
    confirm.request_id = forged.admission_id.clone();
    confirm.existing_admission = Some(forged);
    assert!(matches!(
        port.admit(confirm).await.unwrap(),
        AdmissionOutcome::NotApplied | AdmissionOutcome::Busy
    ));
    assert_eq!(
        port.admit(snapshot_request("new-reservation"))
            .await
            .unwrap(),
        AdmissionOutcome::Admitted { admission }
    );
    let after = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn real_sdk_sqlite_settlement_preserves_exact_joined_attempt_fixture_proof() {
    let directory = tempfile::tempdir().unwrap();
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(launcher(directory.path().join("registry.db"), "settle"))
            .await
            .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let AdmissionOutcome::Admitted { admission } = port
        .admit(snapshot_request("joined-attempt"))
        .await
        .unwrap()
    else {
        panic!("fixture attempt must be admitted by the real SDK");
    };
    let attempt = tokio::spawn(async { tokio::task::yield_now().await });
    attempt.await.unwrap();
    let evidence_id = format!("fixture-task-joined:{}", admission.admission_id);
    let request = SettlementRequest {
        admission: admission.clone(),
        proof: AttemptStoppedProof::AttemptStopped {
            instance_id: admission.instance_id.clone(),
            generation_id: admission.generation_id.clone(),
            execution: admission.execution.clone(),
            evidence_id: evidence_id.clone(),
        },
    };
    let receipt = SettlementOutcome::Applied {
        receipt: SettlementReceipt {
            admission,
            evidence_id,
        },
    };
    assert_eq!(port.settle(request.clone()).await.unwrap(), receipt);
    assert_eq!(port.settle(request).await.unwrap(), receipt);
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            json!({"sessionId":"sqlite-transport-session"}),
        )
        .await
        .unwrap();
    assert_eq!(record["attempt"]["phase"], "settled");
    assert_eq!(record["attempt"]["stoppedProof"]["kind"], "attemptStopped");
}
