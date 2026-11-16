use std::fmt;
use std::fmt::{Debug, Formatter};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::rpc;
use crate::{IncomingMessage, JsRuntimeError, Result, RpcChannel};
use peri_process::ProcessTree;

#[cfg(all(test, unix))]
#[path = "host_lifecycle_test.rs"]
mod host_lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_bounded_stderr_tail() {
        // 载荷在 Node 内生成：Windows 命令行上限 32767 字符，33KB 脚本经 `-e`
        // 传参会以 os error 206（文件名或扩展名过长）拒绝启动。
        let payload = STDERR_TAIL_BYTES + 1024;
        let script = format!("process.stderr.write('prefix-' + 'x'.repeat({payload}));");
        let host =
            JsExecutionHost::spawn(JsProcessSpec::new("node", vec!["-e".into(), script])).unwrap();

        host.wait().await.unwrap();
        let tail = host.stderr_tail().unwrap();
        assert!(tail.len() <= STDERR_TAIL_BYTES);
        assert!(!tail.contains("prefix-"));
        assert!(tail.ends_with('x'));
    }
}

#[path = "host/js_execution_host.rs"]
mod js_execution_host;
pub use js_execution_host::JsExecutionHost;
pub use js_execution_host::JsProcessSpec;
#[cfg(test)]
use js_execution_host::STDERR_TAIL_BYTES;
