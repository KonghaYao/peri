use super::*;

fn node_runtime() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("node"))
        .find(|path| path.is_file())
        .expect("JSONL fixture tests require Node on the trusted launcher PATH")
}

async fn fixture(script: &str) -> (tempfile::TempDir, JsonlSdkDispatcher) {
    fixture_with_reverse(script, None).await
}

async fn fixture_with_reverse(
    script: &str,
    reverse: Option<Arc<dyn RequestTransport>>,
) -> (tempfile::TempDir, JsonlSdkDispatcher) {
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("fixture.cjs");
    tokio::fs::write(&module, script).await.unwrap();
    let dispatcher = JsonlSdkDispatcher::launch_with_reverse_transport(
        SdkDispatcherLaunch {
            executable: node_runtime(),
            module,
            database: directory.path().join("fixture.db"),
            instance_id: "fixture-instance".into(),
            generation_id: "fixture-generation".into(),
        },
        reverse,
    )
    .await
    .unwrap();
    (directory, dispatcher)
}

struct NestedConfirmFixture {
    dispatcher: std::sync::OnceLock<std::sync::Weak<JsonlSdkDispatcher>>,
}

#[async_trait]
impl RequestTransport for NestedConfirmFixture {
    async fn send_request(&self, method: &str, _params: Value) -> Result<Value, AcpError> {
        assert_eq!(method, "session/execute");
        self.dispatcher
            .get()
            .unwrap()
            .upgrade()
            .unwrap()
            .send_request("fixture/confirm", json!({"confirmed":true}))
            .await
    }
}

#[tokio::test]
async fn fixture_full_duplex_nested_execute_confirmation_does_not_deadlock() {
    let reverse = Arc::new(NestedConfirmFixture {
        dispatcher: std::sync::OnceLock::new(),
    });
    let (_directory, dispatcher) = fixture_with_reverse(r#"
let active;
const reply = frame => process.stdout.write(JSON.stringify(frame)+'\n');
require('readline').createInterface({input:process.stdin}).on('line', line => {
  const frame = JSON.parse(line);
  if (frame.method === 'peri/execution/ready') reply({id:frame.id,result:{protocolVersion:2,durability:'durable'}});
  else if (frame.method === 'peri/execution/activate') { active=frame.id; reply({id:'sdk-fixture',method:'session/execute',params:{}}); }
  else if (frame.method === 'fixture/confirm') reply({id:frame.id,result:frame.params});
  else if (frame.id === 'sdk-fixture') reply({id:active,result:frame.result});
});
"#, Some(reverse.clone())).await;
    let dispatcher = Arc::new(dispatcher);
    reverse.dispatcher.set(Arc::downgrade(&dispatcher)).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dispatcher.send_request("peri/execution/activate", json!({})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, json!({"confirmed":true}));
}

#[tokio::test]
async fn fixture_jsonl_correlates_requests_without_claiming_registry_evidence() {
    let (_directory, dispatcher) = fixture(
        r#"
const lines = require('readline').createInterface({input: process.stdin});
lines.on('line', line => {
  const request = JSON.parse(line);
  const result = request.method === 'peri/execution/ready'
    ? {protocolVersion:2,durability:'durable'} : request.params;
  process.stdout.write(JSON.stringify({id:request.id,result})+'\n');
});
"#,
    )
    .await;
    let (first, second) = tokio::join!(
        dispatcher.send_request("fixture/echo", json!({"ticket":"first"})),
        dispatcher.send_request("fixture/echo", json!({"ticket":"second"})),
    );
    assert_eq!(first.unwrap(), json!({"ticket":"first"}));
    assert_eq!(second.unwrap(), json!({"ticket":"second"}));
}

#[tokio::test]
async fn fixture_disconnect_after_request_is_unknown_not_settled() {
    let (_directory, dispatcher) = fixture(r#"
require('readline').createInterface({input:process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method !== 'peri/execution/ready') process.exit(0);
  process.stdout.write(JSON.stringify({id:request.id,result:{protocolVersion:2,durability:'durable'}})+'\n');
});
"#).await;
    let error = dispatcher
        .send_request("peri/execution/settle", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32603);
    assert!(error.message.contains("unknown"));
    assert!(dispatcher
        .send_request("peri/execution/admit", json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn fixture_mismatched_response_poisoned_without_retry() {
    let (_directory, dispatcher) = fixture(r#"
require('readline').createInterface({input:process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  const ready = request.method === 'peri/execution/ready';
  process.stdout.write(JSON.stringify({id:ready?request.id:'wrong',result:ready?{protocolVersion:2,durability:'durable'}:{status:'applied'}})+'\n');
});
"#).await;
    assert_eq!(
        dispatcher
            .send_request("peri/execution/settle", json!({}))
            .await
            .unwrap_err()
            .code,
        -32603
    );
    assert!(dispatcher
        .send_request("peri/execution/admit", json!({}))
        .await
        .is_err());
}

#[test]
fn malformed_protocol_does_not_become_success() {
    assert!(decode_response(r#"{"id":"1","result":{},"error":{"code":-1}}"#, "1").is_err());
    assert!(decode_response(r#"{"id":"2","result":{"status":"applied"}}"#, "1").is_err());
    assert!(decode_response("not-json", "1").is_err());
}

struct OriginalCommandResolveFixture {
    params: Value,
    status: String,
}

#[async_trait]
impl RequestTransport for OriginalCommandResolveFixture {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        assert_eq!(method, "session/work/resolve");
        assert_eq!(params, self.params);
        Ok(json!({"status":self.status}))
    }
}

#[tokio::test]
async fn fixture_jsonl_work_resolve_preserves_full_original_command_and_authoritative_status() {
    use peri_acp_types::session_resources::work::{WorkAction, WorkCommand};
    let command = WorkCommand {
        session_id: "resolve-fixture-session".into(),
        recipient_lifecycle: 17,
        mutation_id: "original-unknown-mutation".into(),
        action: WorkAction::BindResourceOwners {
            expected_revision: 31,
            connections_json: "{\"owner\":\"original\"}".into(),
            authorization_ref: "original-authorization".into(),
        },
    };
    let params = json!({"sessionId":command.session_id,"command":command});
    for status in ["applied", "notApplied", "unknown"] {
        let reverse = Arc::new(OriginalCommandResolveFixture {
            params: params.clone(),
            status: status.into(),
        });
        let (_directory, dispatcher) = fixture_with_reverse(r#"
let active;
const reply = frame => process.stdout.write(JSON.stringify(frame)+'\n');
require('readline').createInterface({input:process.stdin}).on('line', line => {
  const frame = JSON.parse(line);
  if (frame.method === 'peri/execution/ready') reply({id:frame.id,result:{protocolVersion:2,durability:'durable'}});
  else if (frame.method === 'fixture/resolve') { active=frame.id; reply({id:'sdk-original-command',method:'session/work/resolve',params:frame.params}); }
  else if (frame.method === 'fixture/forbidden') { active=frame.id; reply({id:'sdk-original-command',method:'session/work/mutate',params:frame.params}); }
  else if (frame.id === 'sdk-original-command') reply({id:active,result:frame.result ?? {error:frame.error}});
});
"#, Some(reverse)).await;
        assert_eq!(
            dispatcher
                .send_request("fixture/resolve", params.clone())
                .await
                .unwrap(),
            json!({"status":status})
        );
        let rejected = dispatcher
            .send_request("fixture/forbidden", params.clone())
            .await
            .unwrap();
        assert_eq!(rejected["error"]["code"], -32601);
    }
}
