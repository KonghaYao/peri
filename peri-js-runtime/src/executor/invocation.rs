//! 单次 execute 请求与 router tasks 的唯一生命周期 owner。

use serde_json::Value;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::Result;

pub(super) struct Invocation {
    pub(super) request: JoinHandle<Result<Value>>,
    pub(super) request_finished: bool,
    pub(super) cancel: CancellationToken,
    pub(super) routers: JoinSet<()>,
}

impl Invocation {
    pub(super) fn new(request: JoinHandle<Result<Value>>, cancel: CancellationToken) -> Self {
        Self {
            request,
            request_finished: false,
            cancel,
            routers: JoinSet::new(),
        }
    }

    pub(super) async fn shutdown(&mut self) {
        self.cancel.cancel();
        if !self.request_finished {
            self.request.abort();
            let _ = (&mut self.request).await;
            self.request_finished = true;
        }
        tokio::task::yield_now().await;
        self.routers.abort_all();
        while self.routers.join_next().await.is_some() {}
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.request.abort();
        self.routers.abort_all();
    }
}
