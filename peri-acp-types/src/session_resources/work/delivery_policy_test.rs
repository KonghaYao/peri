//! 持久化投递的调度/激活自洽规则（H8：Tui-only 的 Required 提醒合法）。

use crate::session::{MessageActivation, MessagePolicy, MessageRequirement};

#[test]
fn required_delivery_only_rejects_the_passive_contradiction() {
    let required_processing = MessagePolicy {
        requirement: MessageRequirement::Required,
        activation: MessageActivation::EnsureProcessing,
        model_visible: true,
    };
    assert!(super::delivery::disposition_is_consistent(
        &required_processing
    ));

    // H8：Tui-only（不进模型）的 Required 提醒仍是合法投递，不得要求扩大受众。
    let required_client_only = MessagePolicy {
        model_visible: false,
        ..required_processing.clone()
    };
    assert!(super::delivery::disposition_is_consistent(
        &required_client_only
    ));

    // 真正矛盾：必须处理却等不到处理机会。
    let required_passive = MessagePolicy {
        activation: MessageActivation::Passive,
        ..required_processing
    };
    assert!(!super::delivery::disposition_is_consistent(
        &required_passive
    ));

    let optional = MessagePolicy::passive();
    assert!(super::delivery::disposition_is_consistent(&optional));
}
