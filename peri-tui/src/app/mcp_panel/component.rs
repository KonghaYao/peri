use std::any::Any;

use peri_middlewares::mcp::ClientStatus;
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    Frame,
};
use tui_textarea::Input;

use super::{
    super::{
        panel_component::PanelComponent,
        panel_manager::{EventResult, PanelContext, PanelKind},
    },
    App, DetailAction, McpPanel, McpPanelView,
};
use crate::i18n::LcRegistry;

impl PanelComponent for McpPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Mcp
    }

    fn handle_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;

        // confirm_delete mode
        if self.confirm_delete.is_some() {
            match input {
                Input {
                    key: Key::Enter, ..
                } => {
                    self.do_confirm_delete(ctx);
                    if self.servers.is_empty() {
                        EventResult::ClosePanel
                    } else {
                        EventResult::Consumed
                    }
                }
                _ => {
                    self.confirm_delete = None;
                    EventResult::Consumed
                }
            }
        } else {
            let is_server_list = self.view.is_server_list();
            match input {
                Input { key: Key::Up, .. } => {
                    self.do_move_up();
                    EventResult::Consumed
                }
                Input { key: Key::Down, .. } => {
                    self.do_move_down();
                    EventResult::Consumed
                }
                Input {
                    key: Key::Enter, ..
                } => {
                    self.do_enter(ctx);
                    EventResult::Consumed
                }
                Input { key: Key::Esc, .. } => {
                    if is_server_list {
                        EventResult::ClosePanel
                    } else {
                        self.do_back();
                        EventResult::Consumed
                    }
                }
                Input {
                    key: Key::Char('r'),
                    ctrl: true,
                    ..
                } if is_server_list => {
                    self.do_reconnect(ctx);
                    EventResult::Consumed
                }
                Input {
                    key: Key::Char('d'),
                    ctrl: true,
                    ..
                } if is_server_list => {
                    self.do_request_delete();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            }
        }
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &mut PanelContext<'_>) -> EventResult {
        match &self.view {
            McpPanelView::ServerList => {
                self.server_list.handle_scroll(lines, 16);
            }
            McpPanelView::ServerDetail { .. } => {
                if lines > 0 {
                    self.detail_scroll_offset =
                        self.detail_scroll_offset.saturating_add(lines as u16);
                } else {
                    self.detail_scroll_offset =
                        self.detail_scroll_offset.saturating_sub((-lines) as u16);
                }
            }
        }
        EventResult::Consumed
    }

    fn set_scroll_offset(&mut self, offset: u16) {
        self.set_scroll_offset(offset);
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        ctx: &mut PanelContext<'_>,
    ) -> EventResult {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            match &self.view {
                McpPanelView::ServerList => {
                    if self
                        .server_list
                        .handle_mouse_click(mouse.row, mouse.column, area, 2)
                    {
                        return self.handle_key(
                            Input::from(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                            ctx,
                        );
                    }
                }
                McpPanelView::ServerDetail { actions, .. } => {
                    let inner_y = area.y + 4;
                    if mouse.row >= inner_y {
                        let clicked = (mouse.row - inner_y) as usize;
                        if clicked < actions.len() {
                            self.detail_cursor = clicked;
                            let action = actions[clicked].clone();
                            self.do_execute_action(ctx, action);
                            return EventResult::Consumed;
                        }
                    }
                }
            }
        }
        EventResult::NotConsumed
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        match &self.view {
            McpPanelView::ServerList => 16,
            McpPanelView::ServerDetail { .. } => 20,
        }
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::mcp::render_mcp_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, _lc: &LcRegistry) -> Vec<(String, String)> {
        if self.confirm_delete.is_some() {
            return vec![
                ("Enter".to_string(), _lc.tr("key-delete")),
                ("Esc".to_string(), _lc.tr("key-cancel")),
            ];
        }
        if self.view.is_server_list() {
            vec![
                ("\u{2191}\u{2193}".to_string(), _lc.tr("key-move")),
                ("Enter".to_string(), _lc.tr("key-detail")),
                ("Ctrl+R".to_string(), _lc.tr("key-reconnect")),
                ("Ctrl+D".to_string(), _lc.tr("key-delete")),
                ("Esc".to_string(), _lc.tr("key-close")),
            ]
        } else {
            vec![
                ("\u{2191}\u{2193}".to_string(), _lc.tr("key-move")),
                ("Enter".to_string(), _lc.tr("key-execute")),
                ("Esc".to_string(), _lc.tr("key-back")),
            ]
        }
    }
}

impl McpPanel {
    fn do_move_up(&mut self) {
        match &self.view {
            McpPanelView::ServerList => {
                self.server_list.move_cursor(-1);
                self.server_list.ensure_visible(16);
            }
            McpPanelView::ServerDetail { .. } => {
                let max = self.view.action_count().saturating_sub(1);
                self.detail_cursor = self.detail_cursor.saturating_sub(1).min(max);
            }
        }
    }

    fn do_move_down(&mut self) {
        match &self.view {
            McpPanelView::ServerList => {
                self.server_list.move_cursor(1);
                self.server_list.ensure_visible(16);
            }
            McpPanelView::ServerDetail { .. } => {
                let max = self.view.action_count().saturating_sub(1);
                if self.detail_cursor < max {
                    self.detail_cursor += 1;
                }
            }
        }
    }

    fn do_enter(&mut self, ctx: &mut PanelContext<'_>) {
        match &self.view {
            McpPanelView::ServerList => {
                if self.cursor() >= self.servers.len() {
                    return;
                }
                let idx = self.cursor();
                let name = self.servers[idx].name.clone();
                let server = &self.servers[idx];
                let tools = ctx
                    .services
                    .mcp_pool
                    .as_ref()
                    .map(|p| p.get_tools(&name))
                    .unwrap_or_default();
                let resources = ctx
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

                self.view = McpPanelView::ServerDetail {
                    server_name: name,
                    tools,
                    resources,
                    actions,
                    show_tools: false,
                };
                self.detail_cursor = 0;
                self.detail_scroll_offset = 0;
            }
            McpPanelView::ServerDetail { actions, .. } => {
                if self.detail_cursor >= actions.len() {
                    return;
                }
                let action = actions[self.detail_cursor].clone();
                self.do_execute_action(ctx, action);
            }
        }
    }

    fn do_back(&mut self) {
        if self.view.is_server_list() {
            return;
        }
        let name = match &self.view {
            McpPanelView::ServerDetail { server_name, .. } => server_name.clone(),
            _ => String::new(),
        };
        self.view = McpPanelView::ServerList;
        let pos = self
            .servers
            .iter()
            .position(|s| s.name == name)
            .unwrap_or(0);
        self.server_list.move_cursor_to(pos);
        self.server_list.ensure_visible(16);
        self.detail_scroll_offset = 0;
    }

    fn do_request_delete(&mut self) {
        if !self.view.is_server_list() {
            return;
        }
        if self.cursor() >= self.servers.len() {
            return;
        }
        self.confirm_delete = Some(self.servers[self.cursor()].name.clone());
    }

    fn do_confirm_delete(&mut self, ctx: &mut PanelContext<'_>) {
        let name = match self.confirm_delete.take() {
            Some(n) => n,
            None => return,
        };
        if let Some(pool) = ctx.services.mcp_pool.clone() {
            let name_clone = name.clone();
            tokio::spawn(async move {
                pool.remove_server(&name_clone).await;
            });
        }
        let _ = peri_middlewares::mcp::remove_server_from_config(
            std::path::Path::new(&ctx.services.cwd),
            &name,
        );
        self.servers = ctx
            .services
            .mcp_pool
            .as_ref()
            .map(|p| p.all_server_infos())
            .unwrap_or_default();
        self.server_list.set_items(self.servers.clone());
        self.server_list.clamp_cursor();
    }

    fn do_reconnect(&mut self, ctx: &mut PanelContext<'_>) {
        if !self.view.is_server_list() {
            return;
        }
        if self.cursor() >= self.servers.len() {
            return;
        }
        let name = self.servers[self.cursor()].name.clone();
        if let Some(pool) = ctx.services.mcp_pool.clone() {
            let tx = ctx.services.bg_event_tx.clone();
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
                        let _ = tx2.try_send(super::AgentEvent::OAuthAuthorizationNeeded {
                            server_name,
                            authorization_url,
                            callback_tx: dto_tx,
                        });
                    }
                });
            tokio::spawn(async move {
                let result = pool2.reconnect(&name2, Some(oauth_cb)).await;
                let _ = tx
                    .send(super::AgentEvent::McpActionCompleted {
                        server_name: name2,
                        action: "reconnect".to_string(),
                        success: result.is_ok(),
                    })
                    .await;
            });
        }
    }

    fn do_execute_action(&mut self, ctx: &mut PanelContext<'_>, action: DetailAction) {
        let server_name = match &self.view {
            McpPanelView::ServerDetail { server_name, .. } => server_name.clone(),
            _ => return,
        };
        match action {
            DetailAction::ViewTools => {
                if let McpPanelView::ServerDetail {
                    ref mut show_tools, ..
                } = self.view
                {
                    *show_tools = !*show_tools;
                }
            }
            DetailAction::ReAuthenticate => {
                self.do_back();
                self.set_cursor_to_server(&server_name);
                self.do_request_oauth(ctx);
            }
            DetailAction::ClearAuth => {
                self.do_back();
                self.set_cursor_to_server(&server_name);
                let pool = ctx.services.mcp_pool.clone();
                let tx = ctx.services.bg_event_tx.clone();
                let name_clone = server_name.clone();
                if let Some(pool) = pool {
                    tokio::spawn(async move {
                        let result = pool.clear_oauth(&name_clone).await;
                        let _ = tx.try_send(super::AgentEvent::McpActionCompleted {
                            server_name: name_clone,
                            action: "clear_auth".to_string(),
                            success: result.is_ok(),
                        });
                    });
                }
            }
            DetailAction::Reconnect => {
                self.do_back();
                self.set_cursor_to_server(&server_name);
                self.do_reconnect(ctx);
            }
            DetailAction::Disable => {
                self.do_back();
                self.set_cursor_to_server(&server_name);
                Self::toggle_disabled(ctx, &server_name, true);
                self.servers = ctx
                    .services
                    .mcp_pool
                    .as_ref()
                    .map(|p| p.all_server_infos())
                    .unwrap_or_default();
                self.server_list.set_items(self.servers.clone());
                self.server_list.clamp_cursor();
            }
            DetailAction::Enable => {
                self.do_back();
                self.set_cursor_to_server(&server_name);
                Self::toggle_disabled(ctx, &server_name, false);
                self.servers = ctx
                    .services
                    .mcp_pool
                    .as_ref()
                    .map(|p| p.all_server_infos())
                    .unwrap_or_default();
                self.server_list.set_items(self.servers.clone());
                self.server_list.clamp_cursor();
            }
        }
    }

    fn set_cursor_to_server(&mut self, server_name: &str) {
        let pos = self
            .servers
            .iter()
            .position(|s| s.name == server_name)
            .unwrap_or(0);
        self.server_list.move_cursor_to(pos);
    }

    fn do_request_oauth(&mut self, ctx: &mut PanelContext<'_>) {
        if !self.view.is_server_list() {
            return;
        }
        if self.cursor() >= self.servers.len() {
            return;
        }
        let server = &self.servers[self.cursor()];
        if server.transport_type != "http" {
            return;
        }
        let name = server.name.clone();
        if let Some(pool) = ctx.services.mcp_pool.clone() {
            let tx = ctx.services.bg_event_tx.clone();
            let ok_tx = ctx.services.bg_event_tx.clone();
            let err_tx = ctx.services.bg_event_tx.clone();
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
                                let dto_tx = crate::dto_convert::bridge_oauth_callback(callback_tx);
                                let _ = tx.try_send(super::AgentEvent::OAuthAuthorizationNeeded {
                                    server_name,
                                    authorization_url,
                                    callback_tx: dto_tx,
                                });
                            }
                        }),
                    )
                    .await;
                if let Err(e) = result {
                    let _ = err_tx.try_send(super::AgentEvent::OAuthAuthorizationFailed {
                        server_name: name,
                        error: e.to_string(),
                    });
                } else {
                    let _ = ok_tx.try_send(super::AgentEvent::OAuthAuthorizationCompleted {
                        server_name: name,
                    });
                }
            });
        }
    }

    fn toggle_disabled(ctx: &mut PanelContext<'_>, server_name: &str, disabled: bool) {
        let _ = peri_middlewares::mcp::set_server_disabled(
            std::path::Path::new(&ctx.services.cwd),
            server_name,
            disabled,
        );

        if disabled {
            if let Some(pool) = ctx.services.mcp_pool.clone() {
                let name_clone = server_name.to_string();
                tokio::spawn(async move {
                    pool.set_disabled(&name_clone).await;
                });
            }
        } else {
            if let Some(pool) = ctx.services.mcp_pool.clone() {
                let tx = ctx.services.bg_event_tx.clone();
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
                            let _ = tx2.try_send(super::AgentEvent::OAuthAuthorizationNeeded {
                                server_name,
                                authorization_url,
                                callback_tx: dto_tx,
                            });
                        }
                    });
                tokio::spawn(async move {
                    let result = pool2.reconnect(&name2, Some(oauth_cb)).await;
                    let _ = tx
                        .send(super::AgentEvent::McpActionCompleted {
                            server_name: name2,
                            action: "reconnect".to_string(),
                            success: result.is_ok(),
                        })
                        .await;
                });
            }
        }
    }
}
