//! Controller → 契约端口适配（L5：executor 迁入 peri-agent 后，事件发射/
//! 订阅经 `peri-acp-types::event` 端口接入，本层做 Controller 适配）。
//!
//! 依赖方向（§0）：Agent 层执行体只持端口，不引用 Controller 类型；本适配
//! 层在 ACP 宿主侧（协议面），Controller 源码零改动。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    event::ExecutorEvent,
    event::{EventMessage, EventPublisher, EventSubscriber, SubscriptionError},
    runtime::UnstampedEvent,
};

/// Controller → [`EventPublisher`] 适配。
///
/// 语义 = `Controller::publish_event`（补打 session_id / session_seq 后扇出
/// 弹出队列 + 订阅广播）；Agent 层执行体只持端口，不引用 Controller 类型。
pub struct ControllerEventPublisher(pub Arc<peri_controller::Controller>);

impl EventPublisher for ControllerEventPublisher {
    fn publish_event(&self, session_id: &str, source: &UnstampedEvent, event: ExecutorEvent) {
        self.0.publish_event(session_id, source, event);
    }
}

/// Controller 订阅 → [`EventSubscriber`] 适配。
///
/// 错误枚举契约镜像（`SubscriptionError` 在契约层定义），实现方转换，
/// peri-controller 源码零改动。
pub struct ControllerSubscriptionAdapter(pub peri_controller::Subscription);

#[async_trait]
impl EventSubscriber for ControllerSubscriptionAdapter {
    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        self.0.recv().await.map_err(|e| match e {
            peri_controller::SubscriptionError::Lagged(n) => SubscriptionError::Lagged(n),
            peri_controller::SubscriptionError::Closed => SubscriptionError::Closed,
        })
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        self.0.try_recv().map_err(|e| match e {
            peri_controller::SubscriptionError::Lagged(n) => SubscriptionError::Lagged(n),
            peri_controller::SubscriptionError::Closed => SubscriptionError::Closed,
        })
    }
}
