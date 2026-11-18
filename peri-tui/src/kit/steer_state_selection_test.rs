//! 插件选择随输入冻结：撤回恢复与队列内多条输入的选择归属。
//!
//! 选择只随 `input_id` 走；这里验证 TUI 运行期状态不会用“当前全局选择”覆盖它。

use super::{make_command, make_input, make_snapshot, make_state};
use crate::kit::steer_state::SteerCommandKind;
use peri_acp_types::session::{PluginSelection, UserInputQueueReceipt, UserInputQueueSnapshot};

#[test]
fn test_steer_takeback_restores_the_inputs_own_plugin_selection() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    let mut taken_back = make_input("a");
    taken_back.plugin_selections = vec![PluginSelection::new("alpha")];
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: UserInputQueueSnapshot {
                items: Vec::new(),
                ..make_snapshot(2)
            },
            results: Vec::new(),
            taken_back: Some(taken_back),
        },
    );
    let recovered = state
        .recover("s", 1, true)
        .expect("the take back receipt must restore the draft");
    assert_eq!(
        recovered.plugin_selections,
        vec![PluginSelection::new("alpha")],
        "恢复后重新入队必须沿用该输入的选择，不能读取当前全局选择"
    );
}

#[test]
fn test_steer_two_queued_inputs_keep_their_own_plugin_selection() {
    let mut state = make_state();
    let mut first = make_command(SteerCommandKind::Enqueue(make_input("a")));
    first.command_id = "first".to_owned();
    let mut second = make_command(SteerCommandKind::Enqueue(make_input("b")));
    second.command_id = "second".to_owned();
    for (command, plugin) in [(&mut first, "alpha"), (&mut second, "beta@market")] {
        if let SteerCommandKind::Enqueue(input) = &mut command.kind {
            input.plugin_selections = vec![PluginSelection::new(plugin)];
        }
    }
    state.begin(first.clone());
    state.begin(second.clone());
    state.reject(&first, true);
    state.reject(&second, true);

    assert_eq!(
        state
            .recover("s", 1, true)
            .expect("the first rejected input must be recoverable")
            .plugin_selections,
        vec![PluginSelection::new("alpha")]
    );
    assert_eq!(
        state
            .recover("s", 1, true)
            .expect("the second rejected input must be recoverable")
            .plugin_selections,
        vec![PluginSelection::new("beta@market")],
        "队列中两条输入各自保留选择"
    );
    assert!(state.recover("s", 1, true).is_none(), "每条输入只恢复一次");
}
