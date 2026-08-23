use std::{
    ffi::OsString,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use async_trait::async_trait;
use peri_agent::tools::{
    BaseTool, EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher,
    EffectiveToolError, ToolContext,
};
use peri_middlewares::ptc::RunPtcCodeTool;
use serde_json::json;
use tokio_util::sync::CancellationToken;

static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct HomeGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        let lock = HOME_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

struct NoopDispatcher;

#[async_trait]
impl EffectiveToolDispatcher for NoopDispatcher {
    async fn dispatch(
        &self,
        _call: EffectiveToolCall,
        _cancel: CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        unreachable!("test source does not call internal tools")
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        Vec::new()
    }
}

async fn write_cached_adapter(home: &Path) {
    let package = home.join(".peri/ptc/0.2.2/node_modules/@peri-code/ptc");
    tokio::fs::create_dir_all(package.join("dist"))
        .await
        .unwrap();
    tokio::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&json!({
            "name": "@peri-code/ptc",
            "version": "0.2.2",
            "type": "module",
            "main": "dist/index.js",
            "bin": { "peri-ptc": "dist/peri-ptc.js" },
            "periProtocolVersion": 1,
            "periBuildId": "@peri-code/ptc@0.2.2"
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(package.join("dist/index.js"), "export {};\n")
        .await
        .unwrap();
    tokio::fs::write(
        package.join("dist/peri-ptc.js"),
        r#"import readline from 'node:readline';
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'ptc/start') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { ok: true, protocolVersion: 1, buildId: '@peri-code/ptc@0.2.2' } }) + '\n');
  } else if (request.method === 'execute') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32001, message: 'JavaScript execution failed', data: { code: 'TOOL_FAILED' } } }) + '\n');
  }
});
"#,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn run_ptc_code_projects_real_node_failures_without_leaking_user_data() {
    let home = tempfile::tempdir().unwrap();
    write_cached_adapter(home.path()).await;
    let _home = HomeGuard::set(home.path());
    let tool = RunPtcCodeTool::default();
    let error = tool
        .invoke(
            json!({
                "source": "throw new Error('e2e-source-canary');",
                "input": { "secret": "e2e-input-canary" }
            }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(NoopDispatcher),
                "e2e-run-ptc-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(error, "TOOL_FAILED: JavaScript execution failed");
    for forbidden in [
        "e2e-source-canary",
        "e2e-input-canary",
        "throw new Error",
        "stack",
    ] {
        assert!(!error.contains(forbidden), "leaked {forbidden}: {error}");
    }
}
