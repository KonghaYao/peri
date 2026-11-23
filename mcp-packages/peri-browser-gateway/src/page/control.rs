//! 唯一 PageControl writer。状态锁只管准入；操作门覆盖完整的复合命令。

use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::PageHandle;
use crate::cdp::{CdpError, CdpEvent, Controller, PageController};
use crate::error::BrowserError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Free,
    Agent,
    Transition,
    Human { automatic: bool },
    ExternalReserved,
    Closing,
}

impl Role {
    pub fn human(self) -> bool {
        matches!(self, Self::Human { .. })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Lease {
    pub role: Role,
    pub epoch: u64,
    pub quiet_since: Instant,
    pub running: bool,
    pub released: u64,
}

pub(crate) struct PageControl {
    lease: Mutex<Lease>,
    serial: AsyncMutex<WriterSlot>,
    changed: watch::Sender<u64>,
    stop: CancellationToken,
}

struct Writer {
    controller: Controller,
    pump: Option<JoinHandle<()>>,
    intentional_close: CancellationToken,
}

#[derive(Default)]
struct WriterSlot {
    writer: Option<Writer>,
    // 握手独立于调用者。迟到的 socket 先关闭，再允许下一次 attach。
    attaching: Option<JoinHandle<Result<(), CdpError>>>,
    uncertain: bool,
}

pub(crate) struct PageOperation<'a> {
    page: &'a PageHandle,
    writer: MutexGuard<'a, WriterSlot>,
    stop: CancellationToken,
}

impl Deref for PageOperation<'_> {
    type Target = PageHandle;
    fn deref(&self) -> &Self::Target {
        self.page
    }
}

impl Drop for PageOperation<'_> {
    fn drop(&mut self) {
        let mut lease = self.page.control.lease.lock().unwrap();
        lease.running = false;
        lease.quiet_since = Instant::now();
        self.page.control.notify();
    }
}

#[async_trait]
impl PageController for PageOperation<'_> {
    async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        let controller = &self
            .writer
            .writer
            .as_ref()
            .expect("许可带有唯一 writer")
            .controller;
        tokio::select! {
            biased;
            _ = self.stop.cancelled() => Err(CdpError::Closed { url: controller.url().to_owned() }),
            result = controller.command(method, params) => result,
        }
    }

    fn endpoint(&self) -> &str {
        &self.page.endpoint
    }

    async fn next_event(&mut self) -> Option<CdpEvent> {
        None
    }
}

struct Transition<'a> {
    control: &'a PageControl,
    epoch: u64,
    rollback: Role,
}

impl Transition<'_> {
    fn current(&self) -> bool {
        let lease = self.control.lease();
        lease.epoch == self.epoch
            && lease.role == Role::Transition
            && !self.control.stop.is_cancelled()
    }

    fn commit(&self, role: Role, released: bool) -> Result<(), BrowserError> {
        let mut lease = self.control.lease.lock().unwrap();
        if lease.epoch != self.epoch
            || lease.role != Role::Transition
            || self.control.stop.is_cancelled()
        {
            return Err(BrowserError::Closing);
        }
        self.control.update(&mut lease, role);
        lease.released += u64::from(released);
        Ok(())
    }
}

impl Drop for Transition<'_> {
    fn drop(&mut self) {
        let mut lease = self.control.lease.lock().unwrap();
        if lease.epoch == self.epoch && lease.role == Role::Transition {
            self.control.update(&mut lease, self.rollback);
        }
    }
}

impl PageControl {
    pub fn new(changed: watch::Sender<u64>, stop: CancellationToken) -> Self {
        Self {
            lease: Mutex::new(Lease {
                role: Role::Free,
                epoch: 0,
                quiet_since: Instant::now(),
                running: false,
                released: 0,
            }),
            serial: AsyncMutex::new(WriterSlot::default()),
            changed,
            stop,
        }
    }

    pub fn lease(&self) -> Lease {
        *self.lease.lock().unwrap()
    }

    fn notify(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    fn update(&self, lease: &mut Lease, role: Role) {
        lease.role = role;
        lease.epoch += 1;
        lease.quiet_since = Instant::now();
        self.notify();
    }

    fn start(
        &self,
        select: impl FnOnce(Lease) -> Result<Role, BrowserError>,
    ) -> Result<Transition<'_>, BrowserError> {
        let mut lease = self.lease.lock().unwrap();
        if self.stop.is_cancelled() || lease.role == Role::Closing {
            return Err(BrowserError::Closing);
        }
        if lease.role == Role::Transition {
            return Err(BrowserError::Busy);
        }
        let rollback = select(*lease)?;
        self.update(&mut lease, Role::Transition);
        Ok(Transition {
            control: self,
            epoch: lease.epoch,
            rollback,
        })
    }

    async fn attach(
        &self,
        page: &Arc<PageHandle>,
        slot: &mut WriterSlot,
    ) -> Result<bool, BrowserError> {
        if slot
            .writer
            .as_ref()
            .is_some_and(|w| w.controller.reusable())
        {
            return Ok(false);
        }
        slot.close().await?;
        if self.stop.is_cancelled() {
            return Err(BrowserError::Closing);
        }
        page.reset_diagnostic_domains().await;
        page.invalidate_snapshot().await;
        let (ready, result) = oneshot::channel();
        slot.attaching = Some(tokio::spawn(connect_writer(page.clone(), ready)));
        let handoff = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(BrowserError::Closing),
            result = result => result.map_err(|_| CdpError::UnconfirmedClose)??,
        };
        // 交接通过同步取走，避免在 receive 与登记之间丢弃已建立的连接。
        let mut controller = handoff.claim();
        let mut events = controller.take_events();
        let weak: Weak<PageHandle> = Arc::downgrade(page);
        let intentional_close = CancellationToken::new();
        let intentional = intentional_close.clone();
        let pump = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let Some(page) = weak.upgrade() {
                    page.handle_event(event).await;
                }
            }
            if let Some(page) = weak.upgrade() {
                if !intentional.is_cancelled() {
                    page.fence().await;
                }
            }
        });
        slot.writer = Some(Writer {
            controller,
            pump: Some(pump),
            intentional_close,
        });
        Ok(true)
    }

    pub async fn agent<'a>(
        &'a self,
        page: &'a Arc<PageHandle>,
        reclaim: Duration,
    ) -> Result<PageOperation<'a>, BrowserError> {
        let deadline = Instant::now() + reclaim;
        let mut changed = self.changed.subscribe();
        loop {
            match self.lease().role {
                Role::Human { automatic: true } => {
                    self.transition(page, Role::Agent, None, |lease| match lease.role {
                        Role::Human { automatic: true } if !lease.running => Ok(lease.role),
                        _ => Err(BrowserError::Yielded),
                    })
                    .await?;
                }
                Role::Human { .. } => {
                    tokio::select! {
                        _ = self.stop.cancelled() => return Err(BrowserError::Closing),
                        result = tokio::time::timeout_at(deadline, changed.changed()) => if result.is_err() { return Err(BrowserError::Yielded); }
                    }
                    continue;
                }
                Role::ExternalReserved => return Err(BrowserError::Yielded),
                Role::Transition => return Err(BrowserError::Busy),
                Role::Closing => return Err(BrowserError::Closing),
                Role::Agent | Role::Free => {}
            }
            let mut writer = self.serial.lock().await;
            let epoch = {
                let lease = self.lease();
                if !matches!(lease.role, Role::Agent | Role::Free) {
                    continue;
                }
                lease.epoch
            };
            let attached = self.attach(page, &mut writer).await?;
            {
                let mut lease = self.lease.lock().unwrap();
                if lease.epoch == epoch && !self.stop.is_cancelled() {
                    if lease.role == Role::Free {
                        self.update(&mut lease, Role::Agent);
                    }
                    lease.running = true;
                    self.notify();
                    return Ok(PageOperation {
                        page,
                        writer,
                        stop: self.stop.clone(),
                    });
                }
            }
            if attached {
                writer.close().await?;
            }
            return Err(BrowserError::Busy);
        }
    }

    pub async fn human<'a>(
        &'a self,
        page: &'a Arc<PageHandle>,
        epoch: u64,
        client: &CancellationToken,
    ) -> Result<PageOperation<'a>, BrowserError> {
        let mut writer = self.serial.lock().await;
        self.validate_human(epoch, client)?;
        let attached = self.attach(page, &mut writer).await?;
        let result = {
            let mut lease = self.lease.lock().unwrap();
            self.check_human(*lease, epoch, client).map(|()| {
                // 首次真实输入使自动租约变成普通人工租约，但同一 viewer epoch 仍有效。
                lease.role = Role::Human { automatic: false };
                lease.running = true;
            })
        };
        if result.is_err() && attached {
            writer.close().await?;
        }
        result?;
        Ok(PageOperation {
            page,
            writer,
            stop: client.clone(),
        })
    }

    pub fn validate_human(
        &self,
        epoch: u64,
        client: &CancellationToken,
    ) -> Result<(), BrowserError> {
        self.check_human(self.lease(), epoch, client)
    }

    fn check_human(
        &self,
        lease: Lease,
        epoch: u64,
        client: &CancellationToken,
    ) -> Result<(), BrowserError> {
        if self.stop.is_cancelled() || client.is_cancelled() {
            return Err(BrowserError::Closing);
        }
        if !lease.role.human() {
            return Err(if lease.role == Role::Transition {
                BrowserError::Busy
            } else {
                BrowserError::ReadOnly
            });
        }
        if epoch != lease.epoch {
            return Err(BrowserError::StaleView);
        }
        Ok(())
    }

    pub async fn takeover(
        &self,
        page: &Arc<PageHandle>,
        client: &CancellationToken,
        epoch: u64,
    ) -> Result<(), BrowserError> {
        self.transition(
            page,
            Role::Human { automatic: false },
            Some(client),
            |lease| {
                if epoch != lease.epoch {
                    Err(BrowserError::StaleView)
                } else {
                    Ok(lease.role)
                }
            },
        )
        .await
    }

    pub async fn release(&self, page: &Arc<PageHandle>, epoch: u64) -> Result<(), BrowserError> {
        self.transition(page, Role::Free, None, |lease| {
            if lease.epoch != epoch {
                return Err(BrowserError::StaleView);
            }
            if !lease.role.human() {
                return Err(BrowserError::ReadOnly);
            }
            Ok(lease.role)
        })
        .await
    }

    pub async fn reserve(
        &self,
        page: &Arc<PageHandle>,
        client: &CancellationToken,
        epoch: u64,
    ) -> Result<(), BrowserError> {
        self.transition(page, Role::ExternalReserved, Some(client), |lease| {
            if epoch != lease.epoch {
                Err(BrowserError::StaleView)
            } else {
                Ok(lease.role)
            }
        })
        .await
    }

    pub async fn idle(&self, page: &Arc<PageHandle>, viewer: bool, delay: Duration) {
        let role = if viewer {
            Role::Human { automatic: true }
        } else {
            Role::Free
        };
        let _ = self
            .transition(page, role, None, |lease| {
                if lease.running || lease.role != Role::Agent || lease.quiet_since.elapsed() < delay
                {
                    return Err(BrowserError::Busy);
                }
                Ok(lease.role)
            })
            .await;
    }

    pub async fn offer(&self, page: &Arc<PageHandle>) -> Result<(), BrowserError> {
        if self.lease().role == Role::Free {
            return Ok(());
        }
        self.transition(page, Role::Free, None, |lease| match lease.role {
            Role::Agent | Role::Free => Ok(lease.role),
            _ => Err(BrowserError::Yielded),
        })
        .await
    }

    pub async fn close_target(
        &self,
        page: &Arc<PageHandle>,
        human: Option<u64>,
    ) -> Result<(), BrowserError> {
        self.transition(page, Role::Closing, None, |lease| match human {
            Some(epoch) if epoch == lease.epoch && lease.role.human() => Ok(lease.role),
            Some(_) => Err(BrowserError::ReadOnly),
            None if matches!(lease.role, Role::Agent | Role::Free) => Ok(lease.role),
            None => Err(BrowserError::Yielded),
        })
        .await
    }

    async fn transition(
        &self,
        page: &Arc<PageHandle>,
        to: Role,
        client: Option<&CancellationToken>,
        select: impl FnOnce(Lease) -> Result<Role, BrowserError>,
    ) -> Result<(), BrowserError> {
        if client.is_some_and(CancellationToken::is_cancelled) {
            return Err(BrowserError::Closing);
        }
        let mut transition = self.start(select)?;
        let bound = page.connect_timeout + Duration::from_secs(10);
        let mut writer = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(BrowserError::Closing),
            result = tokio::time::timeout(bound, self.serial.lock()) => result.map_err(|_| BrowserError::Busy)?,
        };
        if !transition.current() || client.is_some_and(CancellationToken::is_cancelled) {
            return Err(BrowserError::Closing);
        }
        let released = transition.rollback.human() && to == Role::Free;
        let external = transition.rollback == Role::ExternalReserved;
        // 等到旧操作让出门之后再清快照，避免旧操作发布刚失效的映射。
        page.invalidate_snapshot().await;
        let result = if matches!(to, Role::Agent | Role::Human { .. }) {
            self.attach(page, &mut writer).await.map(|_| ())
        } else {
            writer.close().await.map_err(Into::into)
        };
        if let Err(error) = result {
            transition.rollback = if error.code() == "cdp_close_unconfirmed" {
                Role::Closing
            } else if external {
                Role::ExternalReserved
            } else {
                Role::Free
            };
            return Err(error);
        }
        if !transition.current() || client.is_some_and(CancellationToken::is_cancelled) {
            let result = writer.close().await;
            transition.rollback = if result.is_err() {
                Role::Closing
            } else if external {
                Role::ExternalReserved
            } else {
                Role::Free
            };
            result?;
            return Err(BrowserError::Closing);
        }
        transition.commit(to, released)
    }

    pub fn fence(&self) {
        let mut lease = self.lease.lock().unwrap();
        self.stop.cancel();
        if lease.role != Role::Closing {
            self.update(&mut lease, Role::Closing);
        }
    }

    pub async fn settle(&self) -> Result<(), CdpError> {
        self.serial.lock().await.close().await
    }
}

impl WriterSlot {
    async fn close(&mut self) -> Result<(), CdpError> {
        if let Some(task) = self.attaching.as_mut() {
            self.uncertain |= !matches!(task.await, Ok(Ok(())));
            self.attaching = None;
        }
        close_writer(&mut self.writer).await?;
        if self.uncertain {
            Err(CdpError::UnconfirmedClose)
        } else {
            Ok(())
        }
    }
}

struct WriterHandoff {
    controller: Arc<Mutex<Option<Controller>>>,
    _finished: oneshot::Sender<()>,
}

impl WriterHandoff {
    fn claim(self) -> Controller {
        self.controller
            .lock()
            .unwrap()
            .take()
            .expect("握手只交接一次")
    }
}

async fn connect_writer(
    page: Arc<PageHandle>,
    mut ready: oneshot::Sender<Result<WriterHandoff, CdpError>>,
) -> Result<(), CdpError> {
    let connected = Controller::connect_with_timeout(
        &page.endpoint,
        &page.target_id,
        &page.owner,
        page.connect_timeout,
    )
    .await;
    let mut controller = match connected {
        Ok(controller) => controller,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    // 即使请求已取消，也不能丢弃已完成的握手；不再提交新的 CDP 命令。
    let enabled = tokio::select! {
        biased;
        _ = ready.closed() => return controller.close().await,
        result = controller.command("Page.enable", json!({})) => result,
    };
    if let Err(error) = enabled {
        let closed = controller.close().await;
        let _ = ready.send(Err(if closed.is_ok() {
            error
        } else {
            CdpError::UnconfirmedClose
        }));
        return closed;
    }
    let controller = Arc::new(Mutex::new(Some(controller)));
    let (finished, handoff) = oneshot::channel();
    let _ = ready.send(Ok(WriterHandoff {
        controller: controller.clone(),
        _finished: finished,
    }));
    let _ = handoff.await;
    let unclaimed = controller.lock().unwrap().take();
    if let Some(mut controller) = unclaimed {
        controller.close().await?;
    }
    Ok(())
}

async fn close_writer(writer: &mut Option<Writer>) -> Result<(), CdpError> {
    let Some(slot) = writer.as_mut() else {
        return Ok(());
    };
    slot.intentional_close.cancel();
    let closed = slot.controller.close().await;
    let joined = match slot.pump.as_mut() {
        Some(task) => {
            let result = task.await;
            slot.pump = None;
            result
        }
        None => Ok(()),
    };
    // 若对端未确认，保留已收尾的槽与错误，禁止下一次 attach 或假成功。
    if closed.is_ok() && joined.is_ok() {
        writer.take();
    }
    closed.and(joined.map_err(|_| CdpError::UnconfirmedClose))
}
