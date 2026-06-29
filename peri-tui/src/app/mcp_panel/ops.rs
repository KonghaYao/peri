use peri_middlewares::mcp::ClientStatus;

use super::{AgentEvent, DetailAction, McpPanel, McpPanelView};

impl crate::app::App {
    pub fn mcp_panel_move_up(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            match &panel.view {
                McpPanelView::ServerList => {
                    panel.server_list.move_cursor(-1);
                    panel.server_list.ensure_visible(16);
                }
                McpPanelView::ServerDetail { .. } => {
                    let max = panel.view.action_count().saturating_sub(1);
                    panel.detail_cursor = panel.detail_cursor.saturating_sub(1).min(max);
                }
            }
        }
    }

    pub fn mcp_panel_move_down(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            match &panel.view {
                McpPanelView::ServerList => {
                    panel.server_list.move_cursor(1);
                    panel.server_list.ensure_visible(16);
                }
                McpPanelView::ServerDetail { .. } => {
                    let max = panel.view.action_count().saturating_sub(1);
                    if panel.detail_cursor < max {
                        panel.detail_cursor += 1;
                    }
                }
            }
        }
    }

    pub fn mcp_panel_enter(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            match &panel.view {
                McpPanelView::ServerList => {
                    if panel.server_list.cursor() >= panel.servers.len() {
                        return;
                    }
                    let name = panel.servers[panel.server_list.cursor()].name.clone();
                    let server = &panel.servers[panel.server_list.cursor()];
                    let tools = self
                        .services
                        .mcp_pool
                        .as_ref()
                        .map(|p| p.get_tools(&name))
                        .unwrap_or_default();
                    let resources = self
                        .services
                        .mcp_pool
                        .as_ref()
                        .map(|p| p.get_resources(&name))
                        .unwrap_or_default();

                    let mut actions = vec![DetailAction::ViewTools];
                    if server.transport_type == "http" {
                        actions.push(DetailAction::ReAuthenticate);
                        actions.push(DetailAction::ClearAuth);
                    }
                    if server.status == ClientStatus::Uninitialized {
                        actions = vec![DetailAction::Reconnect];
                    } else {
                        actions.push(DetailAction::Reconnect);
                        if matches!(server.status, ClientStatus::Disabled) {
                            actions.push(DetailAction::Enable);
                        } else {
                            actions.push(DetailAction::Disable);
                        }
                    }

                    panel.view = McpPanelView::ServerDetail {
                        server_name: name,
                        tools,
                        resources,
                        actions,
                        show_tools: false,
                    };
                    panel.detail_cursor = 0;
                    panel.detail_scroll_offset = 0;
                }
                McpPanelView::ServerDetail { ref actions, .. } => {
                    if panel.detail_cursor >= actions.len() {
                        return;
                    }
                    let action = actions[panel.detail_cursor].clone();
                    self.mcp_panel_execute_action(action);
                }
            }
        }
    }

    fn mcp_panel_execute_action(&mut self, action: DetailAction) {
        let server_name = match &self.global_panels.get::<McpPanel>().unwrap().view {
            McpPanelView::ServerDetail { server_name, .. } => server_name.clone(),
            _ => return,
        };
        match action {
            DetailAction::ViewTools => {
                if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
                    if let McpPanelView::ServerDetail {
                        ref mut show_tools, ..
                    } = panel.view
                    {
                        *show_tools = !*show_tools;
                    }
                }
            }
            DetailAction::ReAuthenticate => {
                self.mcp_panel_back();
                self.set_mcp_cursor_to_server(&server_name);
                self.mcp_panel_request_oauth();
            }
            DetailAction::ClearAuth => {
                self.mcp_panel_back();
                self.set_mcp_cursor_to_server(&server_name);
                let pool = self.services.mcp_pool.clone();
                let tx = self.services.bg_event_tx.clone();
                let name_clone = server_name.clone();
                if let Some(pool) = pool {
                    tokio::spawn(async move {
                        let result = pool.clear_oauth(&name_clone).await;
                        let _ = tx.try_send(AgentEvent::McpActionCompleted {
                            server_name: name_clone,
                            action: "clear_auth".to_string(),
                            success: result.is_ok(),
                        });
                    });
                }
            }
            DetailAction::Reconnect => {
                self.mcp_panel_back();
                self.set_mcp_cursor_to_server(&server_name);
                self.mcp_panel_reconnect();
            }
            DetailAction::Disable => {
                self.mcp_panel_back();
                self.set_mcp_cursor_to_server(&server_name);
                self.mcp_panel_toggle_disabled(&server_name, true);
            }
            DetailAction::Enable => {
                self.mcp_panel_back();
                self.set_mcp_cursor_to_server(&server_name);
                self.mcp_panel_toggle_disabled(&server_name, false);
            }
        }
    }

    fn set_mcp_cursor_to_server(&mut self, server_name: &str) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            let pos = panel
                .servers
                .iter()
                .position(|s| s.name == server_name)
                .unwrap_or(0);
            panel.server_list.move_cursor_to(pos);
        }
    }

    pub fn mcp_panel_back(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            if panel.view.is_server_list() {
                return;
            }
            let name = match &panel.view {
                McpPanelView::ServerDetail { server_name, .. } => server_name.clone(),
                _ => String::new(),
            };
            panel.view = McpPanelView::ServerList;
            let pos = panel
                .servers
                .iter()
                .position(|s| s.name == name)
                .unwrap_or(0);
            panel.server_list.move_cursor_to(pos);
            panel.server_list.ensure_visible(16);
            panel.detail_scroll_offset = 0;
        }
    }

    pub fn mcp_panel_request_delete(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            if !panel.view.is_server_list() {
                return;
            }
            if panel.cursor() >= panel.servers.len() {
                return;
            }
            panel.confirm_delete = Some(panel.servers[panel.cursor()].name.clone());
        }
    }

    fn mcp_panel_toggle_disabled(&mut self, server_name: &str, disabled: bool) {
        let _ = peri_middlewares::mcp::set_server_disabled(
            std::path::Path::new(&self.services.cwd),
            server_name,
            disabled,
        );

        if disabled {
            if let Some(pool) = self.services.mcp_pool.clone() {
                let name_clone = server_name.to_string();
                tokio::spawn(async move {
                    pool.set_disabled(&name_clone).await;
                });
            }
        } else {
            if let Some(pool) = self.services.mcp_pool.clone() {
                let tx = self.services.bg_event_tx.clone();
                let pool2 = pool.clone();
                let name2 = server_name.to_string();
                let tx2 = tx.clone();
                let oauth_cb: Box<dyn Fn(peri_middlewares::mcp::OAuthFlowEvent) + Send + Sync> =
                    Box::new(move |ev| {
                        use peri_middlewares::mcp::OAuthFlowEvent;
                        if let OAuthFlowEvent::AuthorizationNeeded {
                            server_name,
                            authorization_url,
                            callback_tx,
                        } = ev
                        {
                            let dto_tx = crate::dto_convert::bridge_oauth_callback(callback_tx);
                            let _ = tx2.try_send(AgentEvent::OAuthAuthorizationNeeded {
                                server_name,
                                authorization_url,
                                callback_tx: dto_tx,
                            });
                        }
                    });
                tokio::spawn(async move {
                    let result = pool2.reconnect(&name2, Some(oauth_cb)).await;
                    let _ = tx
                        .send(AgentEvent::McpActionCompleted {
                            server_name: name2,
                            action: "reconnect".to_string(),
                            success: result.is_ok(),
                        })
                        .await;
                });
            }
        }

        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            panel.servers = self
                .services
                .mcp_pool
                .as_ref()
                .map(|p| p.all_server_infos())
                .unwrap_or_default();
            panel.server_list.set_items(panel.servers.clone());
            panel.server_list.clamp_cursor();
        }
    }

    pub fn mcp_panel_confirm_delete(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            let name = match panel.confirm_delete.take() {
                Some(n) => n,
                None => return,
            };
            if let Some(pool) = self.services.mcp_pool.clone() {
                let name_clone = name.clone();
                tokio::spawn(async move {
                    pool.remove_server(&name_clone).await;
                });
            }
            let _ = peri_middlewares::mcp::remove_server_from_config(
                std::path::Path::new(&self.services.cwd),
                &name,
            );
            panel.servers = self
                .services
                .mcp_pool
                .as_ref()
                .map(|p| p.all_server_infos())
                .unwrap_or_default();
            panel.server_list.set_items(panel.servers.clone());
            panel.server_list.clamp_cursor();
            if panel.servers.is_empty() {
                self.global_panels.close();
            }
        }
    }

    pub fn mcp_panel_cancel_delete(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            panel.confirm_delete = None;
        }
    }

    pub fn mcp_panel_reconnect(&mut self) {
        if let Some(ref mut panel) = self.global_panels.get_mut::<McpPanel>() {
            if !panel.view.is_server_list() {
                return;
            }
            if panel.cursor() >= panel.servers.len() {
                return;
            }
            let name = panel.servers[panel.cursor()].name.clone();
            if let Some(pool) = self.services.mcp_pool.clone() {
                let tx = self.services.bg_event_tx.clone();
                let pool2 = pool.clone();
                let name2 = name.clone();
                let tx2 = tx.clone();
                let oauth_cb: Box<dyn Fn(peri_middlewares::mcp::OAuthFlowEvent) + Send + Sync> =
                    Box::new(move |ev| {
                        use peri_middlewares::mcp::OAuthFlowEvent;
                        if let OAuthFlowEvent::AuthorizationNeeded {
                            server_name,
                            authorization_url,
                            callback_tx,
                        } = ev
                        {
                            let dto_tx = crate::dto_convert::bridge_oauth_callback(callback_tx);
                            let _ = tx2.try_send(AgentEvent::OAuthAuthorizationNeeded {
                                server_name,
                                authorization_url,
                                callback_tx: dto_tx,
                            });
                        }
                    });
                tokio::spawn(async move {
                    let result = pool2.reconnect(&name2, Some(oauth_cb)).await;
                    let _ = tx
                        .send(AgentEvent::McpActionCompleted {
                            server_name: name2,
                            action: "reconnect".to_string(),
                            success: result.is_ok(),
                        })
                        .await;
                });
            }
        }
    }

    pub fn mcp_panel_request_oauth(&mut self) {
        if let Some(panel) = self.global_panels.get::<McpPanel>() {
            if !panel.view.is_server_list() {
                return;
            }
            if panel.cursor() >= panel.servers.len() {
                return;
            }
            let server = &panel.servers[panel.cursor()];
            if server.transport_type != "http" {
                return;
            }
            let name = server.name.clone();
            if let Some(pool) = self.services.mcp_pool.clone() {
                let tx = self.services.bg_event_tx.clone();
                let ok_tx = self.services.bg_event_tx.clone();
                let err_tx = self.services.bg_event_tx.clone();
                tokio::spawn(async move {
                    let result = pool
                        .start_oauth_flow(
                            &name,
                            Box::new(move |ev| {
                                use peri_middlewares::mcp::OAuthFlowEvent;
                                if let OAuthFlowEvent::AuthorizationNeeded {
                                    server_name,
                                    authorization_url,
                                    callback_tx,
                                } = ev
                                {
                                    let dto_tx =
                                        crate::dto_convert::bridge_oauth_callback(callback_tx);
                                    let _ = tx.try_send(AgentEvent::OAuthAuthorizationNeeded {
                                        server_name,
                                        authorization_url,
                                        callback_tx: dto_tx,
                                    });
                                }
                            }),
                        )
                        .await;
                    if let Err(e) = result {
                        let _ = err_tx.try_send(AgentEvent::OAuthAuthorizationFailed {
                            server_name: name,
                            error: e.to_string(),
                        });
                    } else {
                        let _ = ok_tx.try_send(AgentEvent::OAuthAuthorizationCompleted {
                            server_name: name,
                        });
                    }
                });
            }
        }
    }

    pub fn mcp_panel_close(&mut self) {
        self.global_panels.close();
    }
}
