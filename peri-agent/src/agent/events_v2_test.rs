//! Agent 公共路径与 prelude 必须继续导出同一组契约类型和转换函数。

use super::*;
use std::any::TypeId;

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(TypeId::of::<T>(), TypeId::of::<U>());
}

#[test]
fn test_events_v2_public_paths_keep_contract_type_identity() {
    use peri_acp_types::event_v2 as contract;
    assert_same_type::<Event, contract::Event>();
    assert_same_type::<RenderEvent, contract::RenderEvent>();
    assert_same_type::<StateEvent, contract::StateEvent>();
    assert_same_type::<ObserveEvent, contract::ObserveEvent>();
    assert_same_type::<TurnErrorReason, contract::TurnErrorReason>();
    assert_same_type::<EventBus, contract::EventBus>();
    assert_same_type::<EventBusConfig, contract::EventBusConfig>();
    assert_same_type::<EventHandles, contract::EventHandles>();
    assert_same_type::<crate::prelude::Event, contract::Event>();
    assert_same_type::<crate::prelude::RenderEvent, contract::RenderEvent>();
    assert_same_type::<crate::prelude::StateEvent, contract::StateEvent>();
    assert_same_type::<crate::prelude::ObserveEvent, contract::ObserveEvent>();
    assert_same_type::<crate::prelude::TurnErrorReason, contract::TurnErrorReason>();
    assert_same_type::<crate::prelude::EventBus, contract::EventBus>();
    assert_same_type::<crate::prelude::EventBusConfig, contract::EventBusConfig>();
    assert_same_type::<crate::prelude::EventHandles, contract::EventHandles>();
    let _: fn(contract::RenderEvent) -> Option<peri_acp_types::event::ExecutorEvent> =
        render_event_to_executor;
    let _: fn(contract::StateEvent) -> Option<peri_acp_types::event::ExecutorEvent> =
        state_event_to_executor;
    let _: fn(contract::ObserveEvent) -> Option<peri_acp_types::event::ExecutorEvent> =
        observe_event_to_executor;
}
