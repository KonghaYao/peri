// 面板入口方法：open_cron_panel / open_mcp_panel / open_tasks_panel / open_plugin_panel
//
// 合并自原 panel_plugin.rs 拆分后的四个微文件（cron_entry / mcp_entry / tasks_entry / plugin_entry）。
// 四个方法模式一致（impl crate::app::App + 单个 pub 入口），整合为单文件减少导航成本。

impl crate::app::App {
    // ── Cron 面板入口 ──────────────────────────────────────────────────────

    /// 打开 Cron 面板
    pub fn open_cron_panel(&mut self) {
        let tasks: Vec<_> = self
            .services
            .cron
            .scheduler
            .lock()
            .list_tasks()
            .into_iter()
            .cloned()
            .collect();
        if tasks.is_empty() {
            // [TRAP] session_mgr.current_mut().messages.view_messages.push
            // 产生的是 SystemNote 类型 VM，依赖 MessagePipeline 的 ephemeral_notes
            // 锚点机制。禁止改用 BaseMessage::system（会触发系统提示词 hoist 问题）。
            let vm = crate::ui::message_view::MessageViewModel::system(
                self.services.lc.tr("app-no-cron-tasks"),
            );
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(vm);
            self.render_rebuild();
            return;
        }
        let panel = crate::app::CronPanel::new(tasks);
        self.open_panel(crate::app::panel_manager::PanelState::Cron(panel));
    }

    // ── MCP 面板入口 ───────────────────────────────────────────────────────

    /// 打开 MCP 面板
    pub fn open_mcp_panel(&mut self) {
        let infos = self
            .services
            .mcp_pool
            .as_ref()
            .map(|p| p.all_server_infos())
            .unwrap_or_default();
        if infos.is_empty() {
            // [TRAP] session_mgr.current_mut().messages.view_messages.push
            // 产生的是 SystemNote 类型 VM，依赖 MessagePipeline 的 ephemeral_notes
            // 锚点机制。禁止改用 BaseMessage::system（会触发系统提示词 hoist 问题）。
            let vm = crate::ui::message_view::MessageViewModel::system(
                self.services.lc.tr("app-no-mcp-configured"),
            );
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(vm);
            self.render_rebuild();
            return;
        }
        let panel = crate::app::mcp_panel::McpPanel::new(infos);
        self.open_panel(crate::app::panel_manager::PanelState::Mcp(Box::new(panel)));
    }

    // ── Tasks 面板入口 ─────────────────────────────────────────────────────

    /// 打开 Tasks 面板（Agent Threads + Cron Tasks 双标签页）
    pub fn open_tasks_panel(&mut self) {
        use crate::app::tasks_panel::TasksPanel;

        // Load agent threads from ThreadStore (subagent threads with parent_thread_id)
        let agents = self.load_agent_thread_entries();

        // Load cron tasks
        let cron_tasks: Vec<_> = self
            .services
            .cron
            .scheduler
            .lock()
            .list_tasks()
            .into_iter()
            .cloned()
            .collect();

        let panel = TasksPanel::new(agents, cron_tasks);
        self.open_panel(crate::app::panel_manager::PanelState::Tasks(panel));
    }

    /// 从 ThreadStore 加载 agent 线程列表
    fn load_agent_thread_entries(&self) -> Vec<crate::app::tasks_panel::AgentThreadEntry> {
        use crate::app::tasks_panel::AgentThreadEntry;

        let store = self.services.thread_store.clone();
        // [TRAP] tokio::task::block_in_place + block_on 在 TUI 主线程调用。
        // 必须保持 block_in_place 包装，否则在 tokio multi-thread runtime 下会
        // panic（cannot block current thread）。禁止直接 await。
        let threads = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(store.list_threads())
                .unwrap_or_default()
        });

        // Filter to threads matching current cwd
        let cwd = &self.services.cwd;
        let mut entries: Vec<AgentThreadEntry> = threads
            .into_iter()
            .filter(|t| t.cwd == *cwd)
            .map(|t| {
                let title = t.title.clone().unwrap_or_else(|| "(untitled)".to_string());
                AgentThreadEntry {
                    thread_id: t.id.clone(),
                    title,
                    status: "done".to_string(),
                    is_active: false,
                    message_count: t.message_count,
                }
            })
            .collect();

        // Sort: mark current thread as active if we have one
        if let Some(ref current_id) = self.session_mgr.current().current_thread_id {
            for entry in &mut entries {
                if entry.thread_id == *current_id {
                    entry.is_active = true;
                    entry.status = "active".to_string();
                }
            }
            // Sort active first
            entries.sort_by_key(|b| std::cmp::Reverse(b.is_active));
        }

        entries
    }

    // ── Plugin 面板入口 ────────────────────────────────────────────────────

    /// 打开 Plugin 面板
    ///
    /// Pipeline: load_plugin_panel_data() (pure) → open_panel → 后台 spawn
    /// (install_counts / official marketplace 刷新)。
    pub fn open_plugin_panel(&mut self) {
        // [TRAP] 数据加载在纯函数中完成，不持有 &mut self、无 UI 副作用、无 spawn
        let load_result = super::plugin_loader::load_plugin_panel_data();
        let discover_was_empty = load_result.discover_was_empty;

        self.open_panel(crate::app::panel_manager::PanelState::Plugin(Box::new(
            load_result.panel,
        )));

        // 缓存不存在或过期时，后台刷新安装量数据
        if !peri_middlewares::plugin::is_install_counts_cache_valid() {
            let tx = self.services.bg_event_tx.clone();
            super::background::spawn_install_counts_refresh(tx);
        }

        // 首次无缓存时，后台刷新 official marketplace
        // [TRAP] discover_was_empty 驱动后续 official marketplace 刷新（隐式状态机）
        if discover_was_empty {
            // 标记面板加载中状态，避免显示"No plugins available"
            if let Some(ref mut p) = self
                .global_panels
                .get_mut::<crate::app::plugin_panel::PluginPanel>()
            {
                p.discover_loading = true;
            }
            let tx = self.services.bg_event_tx.clone();
            use peri_middlewares::plugin::MarketplaceManager;
            let official_source = peri_middlewares::plugin::MarketplaceSource::GitHub {
                repo: "anthropics/claude-plugins-official".into(),
            };
            let official_name = MarketplaceManager::extract_name(&official_source);
            super::background::spawn_official_marketplace_refresh(tx, official_name);
        }
    }

    pub fn close_plugin_panel(&mut self) {
        self.global_panels
            .close_if(crate::app::panel_manager::PanelKind::Plugin);
    }

    // ── Workflows 面板入口 ──────────────────────────────────────────────────

    /// 打开 Workflows 面板（always opens, starts ACP polling for live updates）
    pub fn open_workflows_panel(&mut self) {
        // Always open the panel (even with empty state).
        let runs = self.global_ui.workflow_tracker.snapshots();
        let panel = crate::app::workflow_panel::WorkflowPanel::new(runs);
        self.open_panel(crate::app::panel_manager::PanelState::Workflow(Box::new(
            panel,
        )));

        // Start ACP polling (1s interval) to pull progress_store data.
        // Kill existing poll if re-opening.
        self.workflow_poll_kill = None; // drop old kill switch → old task exits via send error
        let (poll_tx, poll_rx) = tokio::sync::mpsc::unbounded_channel();
        let (kill_tx, mut kill_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        self.workflow_poll_rx = Some(poll_rx);
        self.workflow_poll_kill = Some(kill_tx);
        self.workflow_polling_active = true;

        if let Some(ref acp_client) = self.acp_client {
            let acp = acp_client.clone();
            let session_id = acp_client.current_session_id().unwrap_or_default();
            tokio::spawn(async move {
                loop {
                    let params = serde_json::json!({ "sessionId": session_id });
                    let result = acp.send_raw_request("workflow/list_runs", params).await;
                    if let Ok(resp) = result {
                        if let Some(runs) = resp.get("runs").and_then(|v| v.as_array()) {
                            let snapshots: Vec<crate::app::workflow_panel::WorkflowRunSnapshot> =
                                runs.iter().map(deserialize_snapshot).collect();
                            if poll_tx.send(snapshots).is_err() {
                                break;
                            }
                        }
                    }

                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                        _ = kill_rx.recv() => break,
                    }
                }
            });
        }
    }
}

/// Deserialize a RunProgress JSON value into WorkflowRunSnapshot.
fn deserialize_snapshot(v: &serde_json::Value) -> crate::app::workflow_panel::WorkflowRunSnapshot {
    crate::app::workflow_panel::WorkflowRunSnapshot {
        run_id: v["run_id"].as_str().unwrap_or("?").to_string(),
        workflow_name: v["workflow_name"].as_str().unwrap_or("?").to_string(),
        status: v["status"]
            .as_str()
            .map(|s| match s {
                "running" => "running",
                "completed" => "completed",
                "failed" => "failed",
                "killed" => "killed",
                _ => "running",
            })
            .unwrap_or("running")
            .to_string(),
        phases: v["phases"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|p| crate::app::workflow_panel::WorkflowPhaseSnapshot {
                        title: p["title"].as_str().unwrap_or("?").to_string(),
                        status: p["status"]
                            .as_str()
                            .map(|s| match s {
                                "pending" => "pending",
                                "active" => "active",
                                "done" => "done",
                                _ => "pending",
                            })
                            .unwrap_or("pending")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        agents: v["agents"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|a| crate::app::workflow_panel::WorkflowAgentSnapshot {
                        agent_id: a["agent_id"].as_u64().unwrap_or(0),
                        label: a["label"].as_str().map(|s| s.to_string()),
                        phase: a["phase"].as_str().map(|s| s.to_string()),
                        status: a["status"]
                            .as_str()
                            .map(|s| match s {
                                "pending" => "pending",
                                "running" => "running",
                                "done" => "done",
                                "dead" => "dead",
                                "skipped" => "skipped",
                                _ => "pending",
                            })
                            .unwrap_or("pending")
                            .to_string(),
                        token_count: a["token_count"].as_u64(),
                        tool_count: a["tool_count"].as_u64(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}
