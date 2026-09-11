//! 连接注册与握手为同一事务；只有当前连接能发布状态。
use super::*;
use crate::{
    jsonrpc::JsonRpcNotification,
    protocol::{notifications::parse_publish_diagnostics, requests::initialize_params},
};

struct Startup<'a> {
    client: &'a LspClient,
    dispatcher: Arc<MessageDispatcher>,
    finished: bool,
}

struct Shutdown<'a> {
    client: &'a LspClient,
    dispatcher: Arc<MessageDispatcher>,
    finished: bool,
}

impl Drop for Shutdown<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Keep the registration reachable for retrying its async join/reap.
            self.dispatcher.begin_close();
        } else {
            let mut connection = self.client.connection.write();
            if connection
                .registered
                .as_ref()
                .is_some_and(|d| Arc::ptr_eq(&d.dispatcher, &self.dispatcher))
            {
                connection.registered = None;
            }
        }
    }
}

impl Startup<'_> {
    fn fail(&mut self, error: &LspError) {
        let mut connection = self.client.connection.write();
        if connection
            .registered
            .as_ref()
            .is_some_and(|d| Arc::ptr_eq(&d.dispatcher, &self.dispatcher))
        {
            connection.registered = None;
            connection.state = ServerState::Error(error.to_string());
        }
        self.finished = true;
    }
}

impl Drop for Startup<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.fail(&LspError::RequestFailed {
                method: "initialize".into(),
                reason: "启动调用已取消".into(),
            });
        }
        // Dropping the last dispatcher reference aborts tasks/kills the process;
        // successful and explicit error paths perform their awaited cleanup below.
    }
}

impl LspClient {
    /// 并发 start 共用一次完整 initialize/initialized 握手。
    pub async fn start(&self, root_uri: &str) -> Result<(), LspError> {
        let _guard = self.start_lock.lock().await;
        if self.is_ready() {
            return Ok(());
        }
        self.do_start(root_uri).await
    }

    async fn do_start(&self, root_uri: &str) -> Result<(), LspError> {
        let previous = {
            let mut connection = self.connection.write();
            connection.state = ServerState::Starting;
            connection.registered.take()
        };
        if let Some(previous) = previous {
            previous.dispatcher.close().await;
            self.diagnostics.clear_all();
        }
        let transport = match crate::jsonrpc::transport::LspTransport::spawn(
            &self.command,
            &self.args,
            &self.env,
        ) {
            Ok(transport) => transport,
            Err(error) => {
                self.connection.write().state = ServerState::Error(error.to_string());
                return Err(error);
            }
        };
        let (dispatcher, rx) = MessageDispatcher::new(transport);
        let dispatcher = Arc::new(dispatcher);
        let diagnostics = self.diagnostics.clone();
        let notification_connection = Arc::downgrade(&self.connection);
        let notification_dispatcher = Arc::downgrade(&dispatcher);
        dispatcher.on_notification(
            "textDocument/publishDiagnostics",
            Box::new(move |params| {
                if let (Some(connection), Some(dispatcher)) = (
                    notification_connection.upgrade(),
                    notification_dispatcher.upgrade(),
                ) {
                    let connection = connection.read();
                    if matches!(
                        connection.state,
                        ServerState::Starting | ServerState::Running
                    ) && connection
                        .registered
                        .as_ref()
                        .is_some_and(|d| Arc::ptr_eq(&d.dispatcher, &dispatcher))
                    {
                        if let Some(params) = parse_publish_diagnostics(&params) {
                            diagnostics.handle_publish_diagnostics(&params);
                        }
                    }
                }
            }),
        );
        let weak_connection = Arc::downgrade(&self.connection);
        let weak_dispatcher = Arc::downgrade(&dispatcher);
        let name = self.name.clone();
        dispatcher.set_on_error(Box::new(move |error| {
            if let (Some(connection), Some(dispatcher)) =
                (weak_connection.upgrade(), weak_dispatcher.upgrade())
            {
                let mut connection = connection.write();
                if matches!(
                    connection.state,
                    ServerState::Starting | ServerState::Running
                ) && connection
                    .registered
                    .as_ref()
                    .is_some_and(|d| Arc::ptr_eq(&d.dispatcher, &dispatcher))
                {
                    tracing::warn!(target: "lsp", server = %name, error = %error, "LSP 服务器错误");
                    connection.state = ServerState::Error(error.to_string());
                }
            }
        }));
        self.connection.write().registered = Some(Arc::new(RegisteredConnection {
            dispatcher: dispatcher.clone(),
            open_files: Mutex::new(HashMap::new()),
        }));
        let mut startup = Startup {
            client: self,
            dispatcher,
            finished: false,
        };
        startup.dispatcher.start_dispatch_loop(rx);
        let result = async {
            let workspace_uri = root_uri
                .parse()
                .unwrap_or_else(|_| "file:///tmp".parse().unwrap());
            let params = initialize_params(
                root_uri.into(),
                vec![lsp_types::WorkspaceFolder {
                    uri: workspace_uri,
                    name: "workspace".into(),
                }],
                self.initialization_options.clone(),
            );
            self.request_on(
                &startup.dispatcher,
                "initialize",
                Some(params),
                self.startup_timeout_ms,
            )
            .await?;
            startup
                .dispatcher
                .send_notification(&JsonRpcNotification::new(
                    "initialized",
                    Some(Value::Object(Default::default())),
                ))
                .await?;
            let mut connection = self.connection.write();
            if connection.state != ServerState::Starting {
                return Err(LspError::TransportClosed);
            }
            connection.state = ServerState::Running;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                startup.finished = true;
                tracing::info!(target: "lsp", server = %self.name, "LSP 服务器初始化成功");
                Ok(())
            }
            Err(error) => {
                startup.fail(&error);
                startup.dispatcher.close().await;
                Err(error)
            }
        }
    }

    pub async fn shutdown(&self) {
        let _guard = self.start_lock.lock().await;
        let dispatcher = {
            let mut connection = self.connection.write();
            connection.state = ServerState::Stopped;
            connection
                .registered
                .as_ref()
                .map(|registered| registered.dispatcher.clone())
        };
        if let Some(dispatcher) = dispatcher {
            let mut shutdown = Shutdown {
                client: self,
                dispatcher,
                finished: false,
            };
            let _ = self
                .request_on(&shutdown.dispatcher, "shutdown", Some(Value::Null), 5_000)
                .await;
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                shutdown
                    .dispatcher
                    .send_notification(&JsonRpcNotification::new("exit", None)),
            )
            .await;
            shutdown.dispatcher.close().await;
            shutdown.finished = true;
        }
    }

    /// 检查重启次数限制并递增计数（同步操作，确保 parking_lot guard 不跨 await）。
    ///
    /// 时间窗退避：计数只在窗口内累计，窗口（默认 60s）过后清零重新累计；
    /// 窗口内计数达到 max_restarts 后返回 ServerCrashed 并进入冷却，
    /// 冷却期内拒绝重启，直到窗口结束。
    fn check_and_increment_restart(&self) -> Result<(), LspError> {
        let mut count = self.restart_count.lock();
        let mut window_start = self.restart_window_start.lock();
        let now = std::time::Instant::now();

        // 窗口已过期：清零计数并开启新窗口；首次重启同样开启窗口
        if let Some(start) = *window_start {
            if now.duration_since(start) >= self.restart_window {
                *count = 0;
                *window_start = Some(now);
            }
        } else {
            *window_start = Some(now);
        }

        if *count >= self.max_restarts {
            return Err(LspError::ServerCrashed {
                server: self.name.clone(),
                restart_count: *count,
                max_restarts: self.max_restarts,
            });
        }
        *count += 1;
        Ok(())
    }

    pub async fn try_restart(&self, root_uri: &str) -> Result<(), LspError> {
        let _guard = self.start_lock.lock().await;
        self.check_and_increment_restart()?;
        self.do_start(root_uri).await
    }
}
