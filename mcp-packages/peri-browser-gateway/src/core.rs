//! 一个 Live MCP session 的唯一端点、页面集合和活动页。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::{watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::BrowserConfig;
use crate::error::BrowserError;
use crate::handoff::Handoff;
use crate::onboarding;
use crate::page::{PageHandle, PageOperation, Role};
use crate::protocol::AgentState;
use crate::provision::{close_target, create_target, probe_engine, EngineProbe, ProvisionSettings};
use crate::session::SettlementError;
use crate::stream::Streams;

mod monitor;

#[derive(Default)]
struct Tabs {
    pages: BTreeMap<String, Arc<PageHandle>>,
    active: Option<String>,
    revision: u64,
}

struct BrowserState {
    endpoint: Option<String>,
    tabs: Tabs,
    viewers: usize,
    visible: usize,
}

pub(crate) struct BrowserCore {
    pub config: Arc<BrowserConfig>,
    pub stop: CancellationToken,
    pub changed: watch::Sender<u64>,
    pub streams: Streams,
    id: String,
    monitor_index: String,
    state: Mutex<BrowserState>,
    structural: AsyncMutex<()>,
    handoff: Arc<Mutex<Option<Handoff>>>,
    service_tasks: Mutex<Option<ServiceTasks>>,
}

struct ServiceTasks {
    idle: JoinHandle<()>,
    stream: JoinHandle<Result<(), SettlementError>>,
}

impl BrowserCore {
    pub fn new(
        config: Arc<BrowserConfig>,
        monitor_index: String,
        stop: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            stop,
            changed: watch::channel(0).0,
            streams: Streams::new(),
            id: uuid::Uuid::new_v4().to_string(),
            monitor_index,
            state: Mutex::new(BrowserState {
                endpoint: None,
                tabs: Tabs::default(),
                viewers: 0,
                visible: 0,
            }),
            structural: AsyncMutex::new(()),
            handoff: Arc::new(Mutex::new(None)),
            service_tasks: Mutex::new(None),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> String {
        format!("{} ({})", self.config.session_name, self.id)
    }

    pub fn summary(&self) -> (usize, AgentState) {
        let (_, pages) = self.tab_snapshot();
        (pages.len(), self.agent_state(&pages))
    }

    fn agent_state(&self, pages: &[Arc<PageHandle>]) -> AgentState {
        if self.handoff.lock().unwrap().is_some() {
            AgentState::WaitingForHuman
        } else if pages.iter().any(|p| {
            let lease = p.control.lease();
            lease.role == Role::Agent && lease.running
        }) {
            AgentState::Driving
        } else {
            AgentState::Idle
        }
    }

    fn notify(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    fn ensure_open(&self) -> Result<(), BrowserError> {
        if self.stop.is_cancelled() {
            Err(BrowserError::Closing)
        } else {
            Ok(())
        }
    }

    fn check_agent(&self) -> Result<(), BrowserError> {
        self.ensure_open()?;
        if self.handoff.lock().unwrap().is_some() {
            Err(BrowserError::WaitingForHuman)
        } else {
            Ok(())
        }
    }

    fn endpoint(&self) -> Result<String, BrowserError> {
        self.state
            .lock()
            .unwrap()
            .endpoint
            .clone()
            .ok_or(BrowserError::NotConnected)
    }

    pub async fn connect(&self, raw_url: &str) -> Result<Value, BrowserError> {
        let canonical = onboarding::normalize_endpoint(raw_url)?;
        let probe = probe_engine(&canonical, self.config.connect_timeout).await;
        if !probe.reachable {
            return Err(BrowserError::ConnectFailed {
                url: canonical,
                reason: probe.error.unwrap_or_else(|| "unreachable".to_owned()),
            });
        }
        let mut state = self.state.lock().unwrap();
        self.ensure_open()?;
        let already = match state.endpoint.as_deref() {
            Some(current) if current == canonical => true,
            Some(current) => {
                return Err(BrowserError::AlreadyConnected {
                    endpoint: current.to_owned(),
                    requested: canonical,
                })
            }
            None => {
                state.endpoint = Some(canonical.clone());
                false
            }
        };
        drop(state);
        self.notify();
        let mut result =
            json!({"connected": true, "endpoint": canonical, "engine": engine_json(&probe)});
        if already {
            result["alreadyConnected"] = json!(true);
        }
        Ok(result)
    }

    pub async fn status(&self) -> Value {
        let endpoint = self.endpoint().ok();
        let probe = match endpoint.as_deref() {
            Some(url) => Some(probe_engine(url, self.config.connect_timeout).await),
            None => None,
        };
        let reachable = probe.as_ref().is_some_and(|probe| probe.reachable);
        if endpoint.is_some() && !reachable {
            let (_, pages) = self.tab_snapshot();
            for page in pages {
                page.fence().await;
            }
        }
        let suggested = onboarding::origin(&self.config.suggested_endpoint);
        let listening = if endpoint.is_none() {
            Some(
                onboarding::probe_recommended(
                    &self.config.suggested_endpoint,
                    self.config.connect_timeout,
                )
                .await,
            )
        } else {
            None
        };
        let install = onboarding::detect_install(&self.config.kitesurf_home, &|key| match key {
            "HOME" => self.config.home.clone(),
            "PATH" => self.config.path.clone(),
            _ => None,
        });
        let next_steps = onboarding::steps_for(
            install.installed,
            listening.unwrap_or(false),
            endpoint.as_deref(),
            reachable,
            &suggested,
        );
        let (active, handles) = self.tab_snapshot();
        let pages = Self::page_views(&active, &handles).await;
        json!({
            "connected": reachable, "endpoint": endpoint, "platform": onboarding::platform().to_json(),
            "kitesurf": install.to_json(), "recommendedEndpoint": {"url": suggested, "listening": listening},
            "nextSteps": next_steps, "owner": self.config.owner, "sessionId": self.id,
            "state": self.agent_state(&handles).as_str(), "pageCount": pages.len(), "activePageId": active, "pages": pages,
            "engine": probe.as_ref().map(engine_json),
            "monitor": if endpoint.is_some() { "connected" } else { "waiting_for_connect" },
            "gatewaySock": null, "monitorIndex": self.monitor_index,
            "yielded": handles.iter().any(|page| yielded(page.control.lease().role)),
        })
    }

    pub async fn list_pages(&self) -> Value {
        let (active, handles) = self.tab_snapshot();
        let pages = Self::page_views(&active, &handles).await;
        json!({"count": pages.len(), "activePageId": active, "pages": pages})
    }

    fn tab_snapshot(&self) -> (Option<String>, Vec<Arc<PageHandle>>) {
        let state = self.state.lock().unwrap();
        let pages = state
            .tabs
            .pages
            .values()
            .filter(|p| !p.expired())
            .cloned()
            .collect::<Vec<_>>();
        let active = state
            .tabs
            .active
            .as_ref()
            .filter(|id| pages.iter().any(|p| &p.id == *id))
            .cloned();
        (active, pages)
    }

    async fn page_views(active: &Option<String>, pages: &[Arc<PageHandle>]) -> Vec<Value> {
        let mut result = Vec::with_capacity(pages.len());
        for page in pages {
            let meta = page.meta().await;
            let role = page.control.lease().role;
            result.push(
                json!({"pageId": page.id, "url": meta.url, "title": meta.title,
                "active": active.as_deref() == Some(&page.id), "yielded": yielded(role),
                "hasController": role == Role::Agent || role.human()}),
            );
        }
        result
    }

    fn resolve(&self, page_id: Option<&str>) -> Result<Arc<PageHandle>, BrowserError> {
        let state = self.state.lock().unwrap();
        let lookup = |id: &str| {
            state
                .tabs
                .pages
                .get(id)
                .filter(|page| !page.expired())
                .cloned()
        };
        match page_id {
            Some(id) => lookup(id).ok_or_else(|| BrowserError::PageNotFound(id.to_owned())),
            None => state
                .tabs
                .active
                .as_deref()
                .and_then(lookup)
                .ok_or(BrowserError::NoActivePage),
        }
    }

    pub async fn tool_page(
        self: &Arc<Self>,
        id: Option<&str>,
    ) -> Result<Arc<PageHandle>, BrowserError> {
        self.check_agent()?;
        if id.is_some() {
            return self.resolve(id);
        }
        self.endpoint()?;
        if let Ok(page) = self.resolve(None) {
            return Ok(page);
        }
        let _structural = self.structural.lock().await;
        self.check_agent()?;
        if let Ok(page) = self.resolve(None) {
            return Ok(page);
        }
        // 有过期页时不自动领回或重放；必须显式新建/选择，避免旧工具重投。
        if !self.state.lock().unwrap().tabs.pages.is_empty() {
            return Err(BrowserError::NoActivePage);
        }
        self.create_page().await
    }

    pub async fn agent_operation<'a>(
        &self,
        page: &'a Arc<PageHandle>,
    ) -> Result<PageOperation<'a>, BrowserError> {
        self.check_agent()?;
        let operation = page.control.agent(page, self.config.reclaim_wait).await?;
        self.check_agent()?;
        Ok(operation)
    }

    pub async fn select_page(&self, id: &str) -> Result<Value, BrowserError> {
        self.check_agent()?;
        let _structural = self.structural.lock().await;
        self.check_agent()?;
        self.switch(id).await
    }

    async fn switch(&self, id: &str) -> Result<Value, BrowserError> {
        let page = self.resolve(Some(id))?;
        if let Ok(previous) = self.resolve(None) {
            if previous.id != page.id {
                self.release_if_human(&previous).await?;
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            self.ensure_open()?;
            if page.expired() {
                return Err(BrowserError::PageNotFound(page.id.clone()));
            }
            if state.tabs.active.as_deref() != Some(id) {
                state.tabs.active = Some(id.to_owned());
                state.tabs.revision += 1;
            }
        }
        self.notify();
        let meta = page.meta().await;
        Ok(json!({"selected": page.id, "pageId": page.id, "url": meta.url, "title": meta.title}))
    }

    async fn release_if_human(&self, page: &Arc<PageHandle>) -> Result<(), BrowserError> {
        let lease = page.control.lease();
        if lease.role.human() {
            page.control.release(page, lease.epoch).await?;
        }
        if lease.role == Role::Transition {
            return Err(BrowserError::Busy);
        }
        Ok(())
    }

    pub async fn new_page(self: &Arc<Self>, url: Option<&str>) -> Result<Value, BrowserError> {
        self.check_agent()?;
        let page = {
            let _structural = self.structural.lock().await;
            self.check_agent()?;
            if let Ok(previous) = self.resolve(None) {
                let role = previous.control.lease().role;
                if role.human() {
                    return Err(BrowserError::Yielded);
                }
                if role == Role::Transition {
                    return Err(BrowserError::Busy);
                }
            }
            self.create_page().await?
        };
        let navigation = match url.map(str::trim).filter(|url| !url.is_empty()) {
            Some(url) => {
                page.control
                    .agent(&page, self.config.reclaim_wait)
                    .await?
                    .navigate_page(url)
                    .await?
            }
            None => Value::Null,
        };
        Ok(self.created(&page, navigation).await)
    }

    async fn create_page(self: &Arc<Self>) -> Result<Arc<PageHandle>, BrowserError> {
        let settings = self.provision_settings()?;
        let target = create_target(&settings).await?;
        let page = PageHandle::new(
            target,
            settings.cdp_url,
            &self.config,
            self.changed.clone(),
            self.stop.clone(),
        );
        {
            let mut state = self.state.lock().unwrap();
            self.ensure_open()?;
            if state
                .tabs
                .pages
                .values()
                .any(|known| known.target_id == page.target_id && !known.expired())
            {
                return Err(BrowserError::Internal(
                    "engine returned an existing target".to_owned(),
                ));
            }
            state.tabs.active = Some(page.id.clone());
            state.tabs.revision += 1;
            state.tabs.pages.insert(page.id.clone(), page.clone());
        }
        self.start_services();
        self.notify();
        Ok(page)
    }

    async fn created(&self, page: &Arc<PageHandle>, navigation: Value) -> Value {
        let meta = page.meta().await;
        json!({"pageId": page.id, "url": meta.url, "title": meta.title, "navigation": navigation, "count": self.tab_snapshot().1.len()})
    }

    pub async fn close_page(&self, id: Option<&str>) -> Result<Value, BrowserError> {
        self.check_agent()?;
        let _structural = self.structural.lock().await;
        self.check_agent()?;
        self.close(self.resolve(id)?, None).await
    }

    async fn close(
        &self,
        page: Arc<PageHandle>,
        human: Option<u64>,
    ) -> Result<Value, BrowserError> {
        let settings = self.provision_settings()?;
        page.control.close_target(&page, human).await?;
        self.ensure_open()?;
        // 一旦发送 close，结果未知时不再次向引擎发送同一写命令。
        close_target(&settings, &page.target_id).await?;
        let mut state = self.state.lock().unwrap();
        state.tabs.pages.remove(&page.id);
        if state.tabs.active.as_deref() == Some(&page.id) {
            state.tabs.active = state
                .tabs
                .pages
                .values()
                .find(|p| !p.expired())
                .map(|p| p.id.clone());
        }
        state.tabs.revision += 1;
        self.notify();
        Ok(
            json!({"closed": page.id, "success": true, "count": state.tabs.pages.values().filter(|p| !p.expired()).count(), "activePageId": state.tabs.active}),
        )
    }

    fn provision_settings(&self) -> Result<ProvisionSettings, BrowserError> {
        Ok(ProvisionSettings {
            cdp_url: self.endpoint()?,
            owner: self.config.owner.clone(),
            lock: self.config.provision_lock.clone(),
            lock_wait: self.config.provision_lock_wait,
            stale_after: self.config.lock_stale_after,
            connect_timeout: self.config.connect_timeout,
        })
    }

    fn start_services(self: &Arc<Self>) {
        let mut tasks = self.service_tasks.lock().unwrap();
        if tasks.is_none() && !self.stop.is_cancelled() {
            *tasks = Some(ServiceTasks {
                idle: tokio::spawn(self.clone().idle_pages()),
                stream: tokio::spawn(crate::stream::run(self.clone())),
            });
        }
    }

    async fn idle_pages(self: Arc<Self>) {
        let mut changed = self.changed.subscribe();
        loop {
            let (active, pages) = self.tab_snapshot();
            let viewers = self.viewer_counts().0 > 0;
            let mut next = tokio::time::Instant::now() + self.config.idle_yield;
            for page in pages {
                if self.stop.is_cancelled() {
                    return;
                }
                let lease = page.control.lease();
                if lease.role != Role::Agent || lease.running {
                    continue;
                }
                let deadline = lease.quiet_since + self.config.idle_yield;
                if deadline <= tokio::time::Instant::now() {
                    if let Ok(_structure) = self.structural.try_lock() {
                        if self.handoff.lock().unwrap().is_none() {
                            page.control
                                .idle(
                                    &page,
                                    viewers && active.as_deref() == Some(&page.id),
                                    self.config.idle_yield,
                                )
                                .await;
                        }
                    }
                } else {
                    next = next.min(deadline);
                }
            }
            tokio::select! {
                _ = self.stop.cancelled() => return,
                _ = changed.changed() => {},
                _ = tokio::time::sleep_until(next) => {},
            }
        }
    }

    pub async fn settle(&self) -> Result<(), SettlementError> {
        let tasks = self.service_tasks.lock().unwrap().take();
        let pages = std::mem::take(&mut self.state.lock().unwrap().tabs.pages);
        let mut failure = None;
        let page_cleanup = futures::future::join_all(
            pages
                .into_values()
                .map(|page| async move { page.control.settle().await }),
        );
        let task_cleanup = async {
            if let Some(ServiceTasks { idle, stream }) = tasks {
                let (idle, stream) = tokio::join!(idle, stream);
                idle.map_err(|_| SettlementError::Transport)?;
                stream.map_err(|_| SettlementError::Transport)??;
            }
            Ok::<_, SettlementError>(())
        };
        let (page_results, task_result) = tokio::join!(page_cleanup, task_cleanup);
        if page_results.into_iter().any(|x| x.is_err()) {
            failure = Some(SettlementError::Unconfirmed);
        }
        task_result?;
        failure.map_or(Ok(()), Err)
    }
}

fn yielded(role: Role) -> bool {
    role.human() || matches!(role, Role::ExternalReserved | Role::Transition)
}

fn engine_json(probe: &EngineProbe) -> Value {
    json!({"reachable": probe.reachable, "version": probe.version, "pageCount": null, "error": probe.error})
}

#[cfg(test)]
#[path = "core_test.rs"]
mod tests;
