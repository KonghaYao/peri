use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::handoff::{self, OfferGuard};
use crate::protocol::{Action, Command, TabInfo};
use crate::stream::FrameMeta;
use crate::{cdp, ws};

pub(crate) use crate::protocol::ViewStamp;

pub(crate) struct View {
    pub stamp: ViewStamp,
    pub page: Option<Arc<PageHandle>>,
    pub role: Role,
    epoch: u64,
}

impl BrowserCore {
    pub fn view(&self) -> View {
        let state = self.state.lock().unwrap();
        let page = state
            .tabs
            .active
            .as_ref()
            .and_then(|id| state.tabs.pages.get(id))
            .filter(|page| !page.expired())
            .cloned();
        let lease = page.as_ref().map(|p| p.control.lease());
        let epoch = lease.map_or(0, |l| l.epoch);
        View {
            stamp: ViewStamp {
                page_id: page.as_ref().map(|p| p.id.clone()),
                view_epoch: format!("{}:{epoch}", state.tabs.revision),
            },
            page,
            role: lease.map_or(Role::Free, |l| l.role),
            epoch,
        }
    }

    pub fn viewer_counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.viewers, state.visible)
    }

    pub fn add_viewer(&self) -> Result<(), BrowserError> {
        let mut state = self.state.lock().unwrap();
        self.ensure_open()?;
        state.viewers += 1;
        state.visible += 1;
        self.notify();
        Ok(())
    }

    pub fn viewer_visibility(&self, now_visible: bool) {
        let mut state = self.state.lock().unwrap();
        if now_visible {
            state.visible += 1;
        } else {
            state.visible -= 1;
        }
        self.notify();
    }

    pub async fn remove_viewer(&self, was_visible: bool) {
        {
            let mut state = self.state.lock().unwrap();
            state.viewers -= 1;
            if was_visible {
                state.visible -= 1;
            }
            self.notify();
        }
        // 不与切页或交接抢着关闭同一 socket；若有新 viewer 到达则保留租约。
        let _structure = self.structural.lock().await;
        if self.stop.is_cancelled() || self.viewer_counts().0 != 0 {
            return;
        }
        if let Some(page) = self.view().page {
            let _ = self.release_if_human(&page).await;
        }
    }

    pub fn stream_tabs(&self) -> (u64, Option<String>, Vec<Arc<PageHandle>>) {
        let state = self.state.lock().unwrap();
        let tabs = &state.tabs;
        let pages = tabs
            .pages
            .values()
            .filter(|p| !p.expired())
            .cloned()
            .collect();
        (tabs.revision, tabs.active.clone(), pages)
    }

    pub async fn monitor_state(&self) -> (ViewStamp, Value) {
        let view = self.view();
        let (_, _, pages) = self.stream_tabs();
        let mut tabs = Vec::with_capacity(pages.len());
        for page in &pages {
            let meta = page.meta().await;
            tabs.push(TabInfo {
                id: page.id.clone(),
                url: meta.url,
                title: meta.title,
                active: view.stamp.page_id.as_deref() == Some(page.id.as_str()),
            });
        }
        let nav = match &view.page {
            Some(page) => {
                let meta = page.meta().await;
                json!({"url": meta.url, "title": meta.title, "canGoBack": meta.can_go_back, "canGoForward": meta.can_go_forward})
            }
            None => Value::Null,
        };
        let handoff = self.handoff.lock().unwrap().as_ref().and_then(|offer| {
            offer.prompt.as_ref().map(|prompt| json!({"requestId": offer.id, "prompt": prompt, "accepted": offer.accepted}))
        });
        let value = json!({"t": "state", "view": view.stamp, "tabs": tabs, "nav": nav, "handoff": handoff,
            "agent": self.agent_state(&pages), "interactive": view.role.human(),
            "pending": view.role == Role::Transition, "externalReserved": view.role == Role::ExternalReserved,
            "canNewTab": self.endpoint().is_ok() && (view.page.is_none() || view.role.human())});
        (view.stamp, value)
    }

    pub fn admit_monitor(
        &self,
        command: &Command,
        client: &CancellationToken,
    ) -> Result<View, BrowserError> {
        self.ensure_open()?;
        if client.is_cancelled() {
            return Err(BrowserError::Closing);
        }
        let view = self.view();
        if view.stamp != command.view {
            return Err(BrowserError::StaleView);
        }
        if command.action.needs_human()
            || (matches!(command.action, Action::NewTab { .. }) && view.page.is_some())
        {
            view.page
                .as_ref()
                .ok_or(BrowserError::NoActivePage)?
                .control
                .validate_human(view.epoch, client)?;
        }
        Ok(view)
    }

    pub async fn monitor_command(
        self: &Arc<Self>,
        command: &Command,
        client: &CancellationToken,
        frame: Option<&FrameMeta>,
    ) -> Result<Value, BrowserError> {
        use Action as A;
        let needs_structure = matches!(
            command.action,
            A::Switch { .. }
                | A::NewTab { .. }
                | A::CloseTab { .. }
                | A::Takeover
                | A::Devtools
                | A::Answer { .. }
                | A::Done { .. }
                | A::Release
        );
        let _structure = if needs_structure {
            Some(self.structural.try_lock().map_err(|_| BrowserError::Busy)?)
        } else {
            None
        };
        let view = self.admit_monitor(command, client)?;
        let offer = self
            .handoff
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| (h.id.clone(), h.prompt.is_some(), h.accepted));
        if let Some((_, human, _)) = &offer {
            if matches!(
                command.action,
                A::Switch { .. } | A::NewTab { .. } | A::CloseTab { .. } | A::Devtools
            ) || (*human && matches!(command.action, A::Takeover))
            {
                return Err(BrowserError::WaitingForHuman);
            }
        }
        match &command.action {
            A::Switch { page_id } => return self.switch(page_id).await,
            A::NewTab { url } => return self.monitor_new_tab(url, view, client).await,
            _ => {}
        }
        let page = view.page.ok_or(BrowserError::NoActivePage)?;
        let epoch = view.epoch;
        match &command.action {
            A::Takeover => {
                page.control.takeover(&page, client, epoch).await?;
                Ok(Value::Null)
            }
            A::Devtools => {
                let url = cdp::page_viewer_url(&page.endpoint, &page.target_id)?;
                let address = url
                    .strip_prefix("ws://")
                    .and_then(|url| url.split_once('?'))
                    .ok_or(BrowserError::Busy)?
                    .0;
                page.control.reserve(&page, client, epoch).await?;
                Ok(
                    json!({"url": format!("devtools://devtools/bundled/inspector.html?ws={address}"), "pageId": page.id}),
                )
            }
            A::CloseTab { page_id } if page_id != &page.id => Err(BrowserError::ReadOnly),
            A::CloseTab { .. } => self.close(page, Some(epoch)).await,
            A::Answer {
                request_id,
                accepted,
            } => {
                let deadline =
                    handoff::validate(&self.handoff.lock().unwrap(), request_id, &page.id)?;
                if !accepted {
                    self.reply_human(request_id, None)?;
                    return Ok(Value::Null);
                }
                tokio::time::timeout_at(deadline, page.control.takeover(&page, client, epoch))
                    .await
                    .map_err(|_| BrowserError::StaleView)??;
                let mut slot = self.handoff.lock().unwrap();
                handoff::validate(&slot, request_id, &page.id)?;
                slot.as_mut().unwrap().accepted = true;
                drop(slot);
                self.notify();
                Ok(Value::Null)
            }
            A::Done { request_id } => self.finish_human(&page, epoch, client, request_id).await,
            A::Release => {
                if let Some((id, true, true)) = offer {
                    return self.finish_human(&page, epoch, client, &id).await;
                }
                page.control.release(&page, epoch).await?;
                Ok(Value::Null)
            }
            action => {
                let op = page.control.human(&page, epoch, client).await?;
                // 活动页也必须仍是 admit 时的页面，而不是仅校验旧页自己的租约。
                self.admit_monitor(command, client)?;
                let xy = |x, y, layout| ws::canvas_to_css(x, y, frame, layout);
                let result = match action {
                    A::Mouse {
                        event,
                        x,
                        y,
                        button,
                        click_count,
                    } => {
                        if frame.is_none() {
                            return Err(BrowserError::StaleView);
                        }
                        let (x, y) = xy(*x, *y, cdp::layout_metrics(&op).await.ok().flatten());
                        cdp::dispatch_mouse(&op, *event, x, y, *button, click_count.unwrap_or(1), 0)
                            .await?
                    }
                    A::Wheel {
                        x,
                        y,
                        delta_x,
                        delta_y,
                    } => {
                        if frame.is_none() {
                            return Err(BrowserError::StaleView);
                        }
                        let (x, y) = xy(*x, *y, cdp::layout_metrics(&op).await.ok().flatten());
                        cdp::dispatch_wheel(&op, x, y, *delta_x, *delta_y).await?
                    }
                    A::Key {
                        event,
                        key,
                        code,
                        text,
                        modifiers,
                    } => {
                        cdp::dispatch_key(&op, *event, key, code, text.as_deref(), *modifiers)
                            .await?
                    }
                    A::Text { text } => cdp::insert_text(&op, text).await?,
                    A::Navigate { url } => op.navigate_page(url).await?,
                    A::History { delta } => op.history(*delta).await?,
                    _ => unreachable!("非输入命令已处理"),
                };
                Ok(result)
            }
        }
    }

    async fn monitor_new_tab(
        self: &Arc<Self>,
        url: &str,
        view: View,
        client: &CancellationToken,
    ) -> Result<Value, BrowserError> {
        if let Some(previous) = view.page {
            previous.control.release(&previous, view.epoch).await?;
        }
        if client.is_cancelled() {
            return Err(BrowserError::Closing);
        }
        let page = self.create_page().await?;
        let navigation = async {
            let epoch = page.control.lease().epoch;
            page.control.takeover(&page, client, epoch).await?;
            let epoch = page.control.lease().epoch;
            let op = page.control.human(&page, epoch, client).await?;
            if url.trim().is_empty() || url == "about:blank" {
                Ok(Value::Null)
            } else {
                op.navigate_page(url).await
            }
        }
        .await;
        Ok(match navigation {
            Ok(value) => self.created(&page, value).await,
            Err(error) => json!({"pageId": page.id, "navigationError": error.code()}),
        })
    }

    pub async fn yield_page(&self) -> Result<Value, BrowserError> {
        let deadline = Instant::now() + self.config.yield_timeout;
        let (page, released, _guard, _receiver) = self.begin_offer(None, deadline).await?;
        let mut changed = self.changed.subscribe();
        let reclaimed = tokio::time::timeout_at(deadline, async {
            loop {
                if page.control.lease().released != released {
                    break;
                }
                if changed.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .is_ok();
        Ok(
            json!({"pageId": page.id, "reclaimed": reclaimed, "status": if reclaimed { "reclaimed" } else { "yield_pending" }}),
        )
    }

    pub async fn request_human(
        &self,
        prompt: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Value, BrowserError> {
        let cap = self.config.request_human_timeout;
        let deadline =
            Instant::now() + timeout_ms.map_or(cap, |ms| Duration::from_millis(ms).min(cap));
        let outcome =
            tokio::time::timeout_at(deadline, self.begin_offer(Some(prompt), deadline)).await;
        let (page, _, _guard, receiver) = match outcome {
            Ok(result) => result?,
            Err(_) => return Ok(json!({"status": "no_human", "reason": "timeout"})),
        };
        match tokio::time::timeout_at(deadline, receiver).await {
            Ok(Ok(Some(shot))) => {
                let meta = page.meta().await;
                Ok(
                    json!({"pageId": page.id, "status": "done", "url": meta.url, "title": meta.title,
                    "screenshot": shot.map(|s| json!({"data": s.data, "mimeType": s.mime_type}))}),
                )
            }
            Ok(Ok(None)) => {
                Ok(json!({"pageId": page.id, "status": "no_human", "reason": "declined"}))
            }
            _ => Ok(json!({"pageId": page.id, "status": "no_human", "reason": "timeout"})),
        }
    }

    async fn begin_offer(
        &self,
        prompt: Option<&str>,
        deadline: Instant,
    ) -> Result<
        (
            Arc<PageHandle>,
            u64,
            OfferGuard,
            oneshot::Receiver<handoff::HumanReply>,
        ),
        BrowserError,
    > {
        self.check_agent()?;
        self.endpoint()?;
        let _structure = self.structural.try_lock().map_err(|_| BrowserError::Busy)?;
        self.check_agent()?;
        let page = self.resolve(None)?;
        let lease = page.control.lease();
        let id = uuid::Uuid::new_v4().to_string();
        let (reply, receiver) = oneshot::channel();
        let guard = OfferGuard {
            slot: self.handoff.clone(),
            id: id.clone(),
            changed: self.changed.clone(),
        };
        *self.handoff.lock().unwrap() = Some(Handoff {
            id,
            page: page.id.clone(),
            prompt: prompt.map(str::to_owned),
            deadline,
            accepted: false,
            reply,
        });
        self.notify();
        if !lease.role.human() {
            // yield 和 request 共用页面门；Ctrl-C/请求取消由 OfferGuard 清横幅。
            let offered = tokio::time::timeout_at(deadline, page.control.offer(&page)).await;
            match offered {
                Ok(result) => result?,
                Err(_) => return Err(BrowserError::Busy),
            }
        }
        Ok((page, lease.released, guard, receiver))
    }

    async fn finish_human(
        &self,
        page: &Arc<PageHandle>,
        epoch: u64,
        client: &CancellationToken,
        id: &str,
    ) -> Result<Value, BrowserError> {
        let deadline = self
            .handoff
            .lock()
            .unwrap()
            .as_ref()
            .filter(|h| {
                h.id == id && h.page == page.id && h.accepted && Instant::now() < h.deadline
            })
            .map(|h| h.deadline)
            .ok_or(BrowserError::StaleView)?;
        // 在同一 Human writer 记录最终页面；不另起 Agent attach。
        let shot = tokio::time::timeout_at(deadline, async {
            let op = page.control.human(page, epoch, client).await?;
            if let Ok(meta) = cdp::navigation_state(&op).await {
                page.set_navigation(&meta).await;
            }
            Ok::<_, BrowserError>(op.take_screenshot().await.ok())
        })
        .await
        .map_err(|_| BrowserError::StaleView)??;
        tokio::time::timeout_at(deadline, page.control.release(page, epoch))
            .await
            .map_err(|_| BrowserError::StaleView)??;
        self.reply_human(id, Some(shot))?;
        Ok(Value::Null)
    }

    fn reply_human(&self, id: &str, reply: handoff::HumanReply) -> Result<(), BrowserError> {
        let mut slot = self.handoff.lock().unwrap();
        if !slot
            .as_ref()
            .is_some_and(|h| h.id == id && Instant::now() < h.deadline)
        {
            return Err(BrowserError::StaleView);
        }
        let result = slot
            .take()
            .unwrap()
            .reply
            .send(reply)
            .map_err(|_| BrowserError::StaleView);
        self.notify();
        result
    }
}
