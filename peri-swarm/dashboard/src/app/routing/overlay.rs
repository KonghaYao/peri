//! Overlay keyboard handling for [`App`].
//!
//! Split out of [`super`] so the routing module stays inside the repository
//! size budget. Routing still reaches this first whenever an overlay is open,
//! before field, page or sidebar shortcuts; that precedence is unchanged.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use peri_tui_components::list_nav::{next_selection, previous_selection};

use crate::app::types::{ActionId, Effect, Intent, Overlay, Page};
use crate::app::App;

impl App {
    pub(super) fn handle_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(overlay) = self.overlay else {
            return Vec::new();
        };
        match overlay {
            Overlay::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
                        self.overlay = None
                    }
                    KeyCode::Up => self.scroll_help(-1),
                    KeyCode::Down => self.scroll_help(1),
                    KeyCode::PageUp => self.page_help(false),
                    KeyCode::PageDown => self.page_help(true),
                    KeyCode::Home => self.jump_help(false),
                    KeyCode::End => self.jump_help(true),
                    _ => {}
                }
                Vec::new()
            }
            Overlay::ConfirmSignOut => match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    Vec::new()
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    self.overlay_cursor = 1 - self.overlay_cursor.min(1);
                    Vec::new()
                }
                KeyCode::Enter => {
                    if self.overlay_cursor == 0 {
                        self.overlay = None;
                        Vec::new()
                    } else {
                        self.activate_action(ActionId::ConfirmSignOut)
                    }
                }
                _ => Vec::new(),
            },
            Overlay::NarrowMenu => match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    Vec::new()
                }
                KeyCode::Up => {
                    self.overlay_cursor = previous_selection(self.overlay_cursor);
                    Vec::new()
                }
                KeyCode::Down => {
                    self.overlay_cursor = next_selection(self.overlay_cursor, Page::ALL.len());
                    Vec::new()
                }
                KeyCode::Home => {
                    self.overlay_cursor = 0;
                    Vec::new()
                }
                KeyCode::End => {
                    self.overlay_cursor = Page::ALL.len() - 1;
                    Vec::new()
                }
                KeyCode::Enter => {
                    let page = Page::ALL[self.overlay_cursor.min(Page::ALL.len() - 1)];
                    self.apply(Intent::SelectMenuPage(page))
                }
                _ => Vec::new(),
            },
            Overlay::ErrorActions => match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    Vec::new()
                }
                KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Tab
                | KeyCode::BackTab => {
                    self.overlay_cursor = 1 - self.overlay_cursor.min(1);
                    Vec::new()
                }
                KeyCode::Enter => {
                    self.overlay = None;
                    if self.overlay_cursor == 0 {
                        self.retry()
                    } else {
                        self.open_settings();
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            },
            Overlay::ConfirmDeleteSession | Overlay::ConfirmCancelTurn => match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    Vec::new()
                }
                KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Tab
                | KeyCode::BackTab => {
                    self.overlay_cursor = 1 - self.overlay_cursor.min(1);
                    Vec::new()
                }
                KeyCode::Enter => {
                    if self.overlay_cursor == 0 {
                        self.overlay = None;
                        Vec::new()
                    } else {
                        let action = if overlay == Overlay::ConfirmDeleteSession {
                            ActionId::ConfirmDeleteSession
                        } else {
                            ActionId::ConfirmCancelTurn
                        };
                        self.activate_action(action)
                    }
                }
                _ => Vec::new(),
            },
        }
    }
}
