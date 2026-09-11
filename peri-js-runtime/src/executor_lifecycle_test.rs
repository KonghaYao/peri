use super::*;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct PendingLaunchProvider {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl PtcArtifactProvider for PendingLaunchProvider {
    async fn launch(&self, _node: &str, cancel: &CancellationToken) -> Result<PtcLaunch> {
        let _drop = DropFlag(self.dropped.clone());
        self.started.notify_one();
        cancel.cancelled().await;
        Err(JsRuntimeError::Cancelled)
    }
    async fn invalidate(&self) -> Result<()> {
        panic!("准备取消不应隔离缓存")
    }
}

async fn pending_launch_stops(cancel_prepare: bool) {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits {
            wall_timeout: if cancel_prepare {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(20)
            },
            ..JsExecutionLimits::default()
        },
        Arc::new(PendingLaunchProvider {
            started: started.clone(),
            dropped: dropped.clone(),
        }),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let child_cancel = cancel.clone();
    let mut task = tokio::spawn(async move {
        executor
            .execute(
                JsExecutionRequest {
                    source: "return 1".into(),
                    input: Value::Null,
                },
                Arc::new(EchoRouter),
                child_cancel,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    if cancel_prepare {
        cancel.cancel();
    }
    let outcome = tokio::time::timeout(Duration::from_millis(500), &mut task).await;
    if outcome.is_err() {
        task.abort();
        let _ = task.await;
    }
    let error = outcome
        .expect("准备阶段必须响应取消或执行deadline")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.code(),
        if cancel_prepare {
            "CANCELLED"
        } else {
            "TIMEOUT"
        }
    );
    assert!(dropped.load(Ordering::SeqCst), "返回前必须释放准备future");
}

#[tokio::test]
async fn test_prepare_cancel_releases_pending_launch() {
    pending_launch_stops(true).await;
}

#[tokio::test]
async fn test_prepare_deadline_releases_pending_launch() {
    pending_launch_stops(false).await;
}

struct ScriptProvider {
    home: tempfile::TempDir,
    script: String,
    invalidated: Arc<AtomicBool>,
}
#[async_trait]
impl PtcArtifactProvider for ScriptProvider {
    async fn launch(&self, node: &str, cancel: &CancellationToken) -> Result<PtcLaunch> {
        let launch = launch_in(node, self.home.path(), &FixtureInstaller, false, cancel).await?;
        tokio::fs::write(&launch.spec.args[0], &self.script).await?;
        Ok(launch)
    }
    async fn invalidate(&self) -> Result<()> {
        self.invalidated.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct CapturedRouter {
    started: Notify,
    token: Mutex<Option<CancellationToken>>,
    dropped: Arc<AtomicBool>,
}
#[async_trait]
impl JsRpcRouter for CapturedRouter {
    async fn route(
        &self,
        method: &str,
        _params: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if method == "fixture/ready" {
            self.started.notified().await;
            return Ok(Value::Null);
        }
        assert_eq!(method, "tool/call");
        let _drop = DropFlag(self.dropped.clone());
        *self.token.lock().unwrap() = Some(cancel);
        self.started.notify_one();
        std::future::pending().await
    }
}

async fn rejected_result_reaps_router(result: Value, expected_code: &str) {
    let script = format!(
        r#"
import readline from 'node:readline';
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
let execution;
readline.createInterface({{ input: process.stdin }}).on('line', line => {{
    const message = JSON.parse(line);
    if (message.method === 'ptc/start') send({{jsonrpc:'2.0',id:message.id,result:{{ok:true,protocolVersion:1,buildId:'@peri-code/ptc@0.2.3'}}}});
    else if (message.method === 'execute') {{
        execution = message.id;
        send({{jsonrpc:'2.0',id:100,method:'tool/call',params:{{}}}});
        send({{jsonrpc:'2.0',id:101,method:'fixture/ready',params:{{}}}});
    }} else if (message.id === 101) send({{jsonrpc:'2.0',id:execution,result:{result}}});
}});
"#
    );
    let dropped = Arc::new(AtomicBool::new(false));
    let router = Arc::new(CapturedRouter {
        started: Notify::new(),
        token: Mutex::new(None),
        dropped: dropped.clone(),
    });
    let invalidated = Arc::new(AtomicBool::new(false));
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits {
            max_result_bytes: 8,
            ..JsExecutionLimits::default()
        },
        Arc::new(ScriptProvider {
            home: tempfile::tempdir().unwrap(),
            script,
            invalidated: invalidated.clone(),
        }),
    )
    .unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        executor.execute(
            JsExecutionRequest {
                source: "fixture".into(),
                input: Value::Null,
            },
            router.clone(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("执行应结束")
    .unwrap_err();
    assert_eq!(error.code(), expected_code);
    let token = router
        .token
        .lock()
        .unwrap()
        .clone()
        .expect("ready响应前必须已捕获路由token");
    assert!(
        token.is_cancelled(),
        "结果验证失败也必须取消已派生的工具token"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "返回前必须join并释放阻塞的router future"
    );
    assert!(
        !invalidated.load(Ordering::SeqCst),
        "执行结果失败不能隔离已握手的缓存"
    );
}

#[tokio::test]
async fn test_malformed_result_cancels_and_reaps_router() {
    rejected_result_reaps_router(json!({"value":1,"logs":false}), "PROTOCOL_ERROR").await;
}

#[tokio::test]
async fn test_over_budget_result_cancels_and_reaps_router() {
    rejected_result_reaps_router(
        json!({"value":"long result exceeds budget","logs":[]}),
        "RESOURCE_LIMIT",
    )
    .await;
}

async fn interrupted_handshake_preserves_cache(cancel_handshake: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let ready = fixture.path().join("handshake-ready");
    let invalidated = Arc::new(AtomicBool::new(false));
    let script = format!(
        "import fs from 'node:fs'; process.stdin.resume(); process.stdin.once('data',()=>fs.writeFileSync({},String(process.pid)));",
        serde_json::to_string(&ready).unwrap()
    );
    let executor = JsExecutor::with_artifact_provider(
        "node",
        JsExecutionLimits {
            wall_timeout: if cancel_handshake {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(2)
            },
            ..JsExecutionLimits::default()
        },
        Arc::new(ScriptProvider {
            home: tempfile::tempdir().unwrap(),
            script,
            invalidated: invalidated.clone(),
        }),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            executor
                .execute(
                    JsExecutionRequest {
                        source: "not-sent".into(),
                        input: Value::Null,
                    },
                    Arc::new(EchoRouter),
                    cancel,
                )
                .await
        }
    });
    let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(&ready).await {
                if let Ok(pid) = value.parse() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Node必须已收到ptc/start且保持未响应");
    if cancel_handshake {
        cancel.cancel();
    }
    let error = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("握手中取消或wall deadline不能等待15秒协议超时")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.code(),
        if cancel_handshake {
            "CANCELLED"
        } else {
            "TIMEOUT"
        }
    );
    assert!(
        !invalidated.load(Ordering::SeqCst),
        "用户取消或wall deadline不代表缓存损坏"
    );
    assert!(pid > 0);
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks existence of the fixture's recorded positive pid.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "返回时握手进程必须已回收"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[tokio::test]
async fn test_handshake_cancel_reaps_process_without_quarantining_cache() {
    interrupted_handshake_preserves_cache(true).await;
}

#[tokio::test]
async fn test_handshake_deadline_reaps_process_without_quarantining_cache() {
    interrupted_handshake_preserves_cache(false).await;
}
