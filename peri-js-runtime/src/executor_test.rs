use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::artifact::{launch_in, Installer, PtcArtifactProvider, PtcLaunch};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    normalize_execute_response_error, validate_handshake, JsExecutionLimits, JsExecutionRequest,
    JsExecutor, JsRpcRouter,
};
use crate::{JsExecutionFailure, JsRuntimeError, JsonRpcError, ResourceKind, Result};

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_local_cache_spawn_failure_invalidates() {
    struct SpawnFailureProvider {
        home: tempfile::TempDir,
        invalidated: Arc<AtomicBool>,
    }

    #[async_trait]
    impl PtcArtifactProvider for SpawnFailureProvider {
        async fn launch(&self, _node: &str) -> Result<PtcLaunch> {
            launch_in(
                "/definitely/missing/peri-node",
                self.home.path(),
                &FixtureInstaller,
                false,
            )
            .await
        }

        async fn invalidate(&self) -> Result<()> {
            self.invalidated.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let invalidated = Arc::new(AtomicBool::new(false));
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits::default(),
        Arc::new(SpawnFailureProvider {
            home: tempfile::tempdir().unwrap(),
            invalidated: Arc::clone(&invalidated),
        }),
    )
    .unwrap();

    executor
        .execute(
            JsExecutionRequest {
                source: "return 1;".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(invalidated.load(Ordering::SeqCst));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_rpc_failure_after_handshake_does_not_invalidate() {
    struct ExecuteFailureProvider {
        home: tempfile::TempDir,
        invalidated: Arc<AtomicBool>,
    }

    #[async_trait]
    impl PtcArtifactProvider for ExecuteFailureProvider {
        async fn launch(&self, node: &str) -> Result<PtcLaunch> {
            let launch = launch_in(node, self.home.path(), &FixtureInstaller, false).await?;
            tokio::fs::write(
                Path::new(&launch.spec.args[0]),
                b"process.stdin.resume(); let started=false; process.stdin.on('data', data => { if (!started) { started=true; process.stdout.write('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true,\"protocolVersion\":1,\"buildId\":\"@peri-code/ptc@0.2.2\"}}\\n'); } else { process.exit(1); } });",
            )
            .await?;
            Ok(launch)
        }

        async fn invalidate(&self) -> Result<()> {
            self.invalidated.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let invalidated = Arc::new(AtomicBool::new(false));
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits::default(),
        Arc::new(ExecuteFailureProvider {
            home: tempfile::tempdir().unwrap(),
            invalidated: Arc::clone(&invalidated),
        }),
    )
    .unwrap();

    executor
        .execute(
            JsExecutionRequest {
                source: "process.exit(1);".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(!invalidated.load(Ordering::SeqCst));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_handshake_failure_invalidates_local_cache_after_cleanup() {
    struct BrokenHandshakeProvider {
        home: tempfile::TempDir,
        invalidated: Arc<AtomicBool>,
    }

    #[async_trait]
    impl PtcArtifactProvider for BrokenHandshakeProvider {
        async fn launch(&self, node: &str) -> Result<PtcLaunch> {
            let launch = launch_in(node, self.home.path(), &FixtureInstaller, false).await?;
            let entry = Path::new(&launch.spec.args[0]);
            tokio::fs::write(
                entry,
                b"process.stdin.resume(); process.stdin.once('data', () => process.stdout.write('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true,\"protocolVersion\":9,\"buildId\":\"broken\"}}\\n'));",
            )
            .await?;
            Ok(launch)
        }

        async fn invalidate(&self) -> Result<()> {
            crate::artifact::quarantine(self.home.path()).await?;
            self.invalidated.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let invalidated = Arc::new(AtomicBool::new(false));
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits::default(),
        Arc::new(BrokenHandshakeProvider {
            home: tempfile::tempdir().unwrap(),
            invalidated: Arc::clone(&invalidated),
        }),
    )
    .unwrap();
    let error = executor
        .execute(
            JsExecutionRequest {
                source: "return 1;".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "PROTOCOL_ERROR");
    assert!(invalidated.load(Ordering::SeqCst));
}

struct EchoRouter;

struct FixtureInstaller;

#[async_trait]
impl Installer for FixtureInstaller {
    async fn install(&self, staging: &Path) -> std::io::Result<bool> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../npm-packages/@peri-ptc");
        let package = staging.join("node_modules/@peri-code/ptc");
        tokio::fs::create_dir_all(package.join("dist")).await?;
        tokio::fs::copy(source.join("package.json"), package.join("package.json")).await?;
        for file in ["peri-ptc.js", "index.js"] {
            tokio::fs::copy(
                source.join("dist").join(file),
                package.join("dist").join(file),
            )
            .await?;
        }
        Ok(true)
    }
}

struct FixtureProvider {
    home: tempfile::TempDir,
}

#[async_trait]
impl PtcArtifactProvider for FixtureProvider {
    async fn launch(&self, node: &str) -> Result<PtcLaunch> {
        launch_in(node, self.home.path(), &FixtureInstaller, false).await
    }

    async fn invalidate(&self) -> Result<()> {
        crate::artifact::quarantine(self.home.path()).await
    }
}

fn fixture_provider() -> Arc<dyn PtcArtifactProvider> {
    Arc::new(FixtureProvider {
        home: tempfile::tempdir().unwrap(),
    })
}

fn test_executor(program: &str) -> JsExecutor {
    JsExecutor::with_artifact_provider(program, JsExecutionLimits::default(), fixture_provider())
        .unwrap()
}

fn test_executor_with_limits(program: &str, limits: JsExecutionLimits) -> Result<JsExecutor> {
    JsExecutor::with_artifact_provider(program, limits, fixture_provider())
}

#[test]
fn test_handshake_rejects_protocol_build_and_ok_mismatch() {
    assert!(validate_handshake(&json!({
        "ok": true,
        "protocolVersion": 1,
        "buildId": "@peri-code/ptc@0.2.2"
    }))
    .is_ok());
    for response in [
        json!({"ok": false, "protocolVersion": 1, "buildId": "@peri-code/ptc@0.2.2"}),
        json!({"ok": true, "protocolVersion": 2, "buildId": "@peri-code/ptc@0.2.2"}),
        json!({"ok": true, "protocolVersion": 1, "buildId": "malicious"}),
    ] {
        assert!(validate_handshake(&response).is_err());
    }
}

#[async_trait]
impl JsRpcRouter for EchoRouter {
    async fn route(
        &self,
        method: &str,
        params: Option<Value>,
        _cancel: CancellationToken,
    ) -> Result<Value> {
        assert_eq!(method, "tool/call");
        Ok(params.unwrap()["input"].clone())
    }
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_supports_concurrent_tool_promises_and_completion() {
    let result = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "const values = await Promise.all([tools.Read({ n: 1 }), tools.Read({ n: 2 })]); console.log('done'); return values;".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result.value, json!([{ "n": 1 }, { "n": 2 }]));
    assert_eq!(result.logs, vec!["done"]);
}

struct BlockingRouter {
    cancelled: Arc<AtomicBool>,
}

#[async_trait]
impl JsRpcRouter for BlockingRouter {
    async fn route(
        &self,
        _method: &str,
        _params: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        cancel.cancelled().await;
        self.cancelled.store(true, Ordering::SeqCst);
        Err(crate::JsRuntimeError::Cancelled)
    }
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_cancels_unawaited_tool_calls_before_completion() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let result = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "tools.Bash({ command: 'slow' }); await new Promise((resolve) => setTimeout(resolve, 20)); return 'done';".into(),
                input: Value::Null,
            },
            Arc::new(BlockingRouter {
                cancelled: Arc::clone(&cancelled),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result.value, json!("done"));
    assert!(cancelled.load(Ordering::SeqCst));
}

struct AbortRouter {
    slow_cancelled: Arc<AtomicBool>,
}

#[async_trait]
impl JsRpcRouter for AbortRouter {
    async fn route(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let params = params.unwrap();
        match method {
            "tool/call" if params["input"]["kind"] == "slow" => {
                cancel.cancelled().await;
                self.slow_cancelled.store(true, Ordering::SeqCst);
                Err(crate::JsRuntimeError::Cancelled)
            }
            "tool/call" => Ok(params["input"].clone()),
            "tool/cancel" => Ok(Value::Null),
            _ => unreachable!(),
        }
    }
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_abort_signal_cancels_only_target_tool_invocation() {
    let slow_cancelled = Arc::new(AtomicBool::new(false));
    let result = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "const controller = new AbortController(); const slow = tools.Read({ kind: 'slow' }, { signal: controller.signal }).catch((error) => error.name); const fast = tools.Read({ kind: 'fast' }); controller.abort(); return [await slow, await fast];".into(),
                input: Value::Null,
            },
            Arc::new(AbortRouter {
                slow_cancelled: Arc::clone(&slow_cancelled),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result.value, json!(["AbortError", { "kind": "fast" }]));
    assert!(slow_cancelled.load(Ordering::SeqCst));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_rejects_source_over_limit_before_spawn() {
    let limits = JsExecutionLimits {
        max_source_bytes: 1,
        ..JsExecutionLimits::default()
    };
    let error = test_executor_with_limits("missing-node-program", limits)
        .unwrap()
        .execute(
            JsExecutionRequest {
                source: "too large".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        JsRuntimeError::ResourceLimit {
            resource: ResourceKind::SourceBytes,
            ..
        }
    ));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_hard_timeout_kills_busy_loop() {
    let limits = JsExecutionLimits {
        wall_timeout: std::time::Duration::from_millis(100),
        ..JsExecutionLimits::default()
    };
    let error = test_executor_with_limits("node", limits)
        .unwrap()
        .execute(
            JsExecutionRequest {
                source: "while (true) {}".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "TIMEOUT");
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_redacts_javascript_exception() {
    let error = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "throw new Error('ptc-canary-value')".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(!error.to_string().contains("ptc-canary-value"));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_propagates_cancellation() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "await new Promise(() => {});".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            cancel,
        )
        .await;

    assert!(matches!(result, Err(crate::JsRuntimeError::Cancelled)));
}

fn remote_execute_error(code: i32, message: &str, data: Option<Value>) -> JsRuntimeError {
    JsRuntimeError::RpcResponse(JsonRpcError {
        code,
        message: message.into(),
        data,
    })
}

#[test]
fn test_execute_response_normalizes_allowlisted_tool_failure() {
    let error = normalize_execute_response_error(remote_execute_error(
        -32001,
        "JavaScript execution failed",
        Some(json!({ "code": "TOOL_FAILED" })),
    ));
    assert!(matches!(
        &error,
        JsRuntimeError::ExecutionFailed(JsExecutionFailure::ToolFailed)
    ));
    assert_eq!(error.code(), "TOOL_FAILED");
    assert_eq!(error.public_message(), "JavaScript execution failed");
}

#[test]
fn test_execute_response_normalizes_allowlisted_resource_limit() {
    let error = normalize_execute_response_error(remote_execute_error(
        -32001,
        "JavaScript resource limit exceeded",
        Some(json!({ "code": "RESOURCE_LIMIT" })),
    ));
    assert_eq!(error.code(), "RESOURCE_LIMIT");
    assert_eq!(error.public_message(), "JavaScript resource limit exceeded");
}

#[test]
fn test_execute_response_normalizes_allowlisted_timeout_and_cancellation() {
    for (code, message, expected) in [
        (
            "TIMEOUT",
            "JavaScript execution timed out",
            JsExecutionFailure::Timeout,
        ),
        (
            "CANCELLED",
            "JavaScript execution cancelled",
            JsExecutionFailure::Cancelled,
        ),
    ] {
        let error = normalize_execute_response_error(remote_execute_error(
            -32001,
            message,
            Some(json!({ "code": code })),
        ));
        assert!(matches!(
            &error,
            JsRuntimeError::ExecutionFailed(failure) if failure == &expected
        ));
        assert_eq!(error.code(), code);
        assert_eq!(error.public_message(), message);
    }
}

#[test]
fn test_execute_response_rejects_unknown_code_without_leaking_remote_data() {
    let error = normalize_execute_response_error(remote_execute_error(
        -32001,
        "remote-canary",
        Some(json!({ "code": "UNKNOWN", "source": "source-canary", "stack": "stack-canary" })),
    ));
    let projection = format!("{}: {}", error.code(), error.public_message());
    assert_eq!(projection, "PROTOCOL_ERROR: JavaScript RPC protocol error");
    assert!(!projection.contains("canary"));
}

#[test]
fn test_execute_response_rejects_allowlisted_code_with_wrong_message() {
    for (code, message) in [
        ("TOOL_FAILED", "message-canary"),
        ("TIMEOUT", "JavaScript execution cancelled"),
        ("CANCELLED", "JavaScript execution timed out"),
    ] {
        let error = normalize_execute_response_error(remote_execute_error(
            -32001,
            message,
            Some(json!({ "code": code })),
        ));
        assert_eq!(error.code(), "PROTOCOL_ERROR");
        assert_eq!(error.public_message(), "JavaScript RPC protocol error");
    }
}

#[test]
fn test_execute_response_rejects_missing_or_non_object_data() {
    for data in [None, Some(json!("data-canary"))] {
        let error = normalize_execute_response_error(remote_execute_error(
            -32001,
            "JavaScript execution failed",
            data,
        ));
        assert_eq!(error.code(), "PROTOCOL_ERROR");
        assert_eq!(error.public_message(), "JavaScript RPC protocol error");
    }
}

async fn execute_source(source: &str) -> JsRuntimeError {
    test_executor("node")
        .execute(
            JsExecutionRequest {
                source: source.into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_supports_node_dynamic_import() {
    let result = test_executor("node")
        .execute(
            JsExecutionRequest {
                source: "const crypto = await import('node:crypto'); return crypto.createHash('sha256').update('perihelion').digest('hex');".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.value,
        json!("fd821357caaebb76f30b4f60527103744172f2d5488fe99a369b47e04d8a6e0b")
    );
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_require_as_safe_tool_failure() {
    let error = execute_source("require('node:crypto');").await;
    assert_eq!(error.code(), "TOOL_FAILED");
    assert_eq!(error.public_message(), "JavaScript execution failed");
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_exception_without_canary_leak() {
    let error = execute_source("throw new Error('executor-canary');").await;
    assert_eq!(error.code(), "TOOL_FAILED");
    assert_eq!(error.to_string(), "JavaScript execution failed");
    assert!(!error.to_string().contains("executor-canary"));
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_syntax_error_as_tool_failure() {
    assert_eq!(
        execute_source("this is invalid !!!").await.code(),
        "TOOL_FAILED"
    );
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_bigint_as_tool_failure() {
    assert_eq!(execute_source("return 42n;").await.code(), "TOOL_FAILED");
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_circular_value_as_tool_failure() {
    assert_eq!(
        execute_source("const value = {}; value.self = value; return value;")
            .await
            .code(),
        "TOOL_FAILED"
    );
}

#[cfg_attr(windows, serial_test::serial)]
#[tokio::test]
async fn test_execute_classifies_adapter_result_limit() {
    let limits = JsExecutionLimits {
        max_result_bytes: 2,
        ..JsExecutionLimits::default()
    };
    let error = test_executor_with_limits("node", limits)
        .unwrap()
        .execute(
            JsExecutionRequest {
                source: "return 'oversized';".into(),
                input: Value::Null,
            },
            Arc::new(EchoRouter),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "RESOURCE_LIMIT");
    assert_eq!(error.public_message(), "JavaScript resource limit exceeded");
}
