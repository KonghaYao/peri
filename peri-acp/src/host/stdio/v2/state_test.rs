use super::{Attach, Foreground, Slot};

fn running() -> Slot {
    let mut slot = Slot::new(Attach::Attached);
    assert!(slot.begin_turn());
    slot
}

#[test]
fn turn_start_publishes_running_once_and_bumps_the_epoch() {
    let mut slot = Slot::new(Attach::Attached);
    assert_eq!(slot.foreground, Foreground::Idle);
    // Idle sessions are not blocked on user action.
    assert_eq!(slot.block(), None);
    assert!(slot.begin_turn());
    assert_eq!(slot.foreground, Foreground::Running);
    assert_eq!(slot.epoch, 1);
    // A second start signal for the same turn is not a new turn.
    assert!(!slot.begin_turn());
    assert_eq!(slot.epoch, 1);
}

#[test]
fn concurrent_blocking_requests_only_return_to_running_after_the_last_one() {
    let mut slot = running();
    let (epoch, publish) = slot.block().expect("foreground turn blocks");
    assert!(publish, "the first request publishes requires_action");
    assert_eq!(slot.foreground, Foreground::RequiresAction);
    let (epoch2, publish2) = slot.block().expect("same turn, second request");
    assert_eq!(epoch, epoch2);
    assert!(!publish2, "requires_action is already published");
    assert_eq!(slot.blocking, 2);

    assert!(
        !slot.unblock(epoch),
        "one of two completions must not resume the turn"
    );
    assert_eq!(slot.foreground, Foreground::RequiresAction);
    assert!(slot.unblock(epoch));
    assert_eq!(slot.foreground, Foreground::Running);
    assert_eq!(slot.blocking, 0);
}

#[test]
fn a_completion_from_a_cancelled_turn_cannot_publish_running() {
    let mut slot = running();
    let (epoch, _) = slot.block().expect("block");
    slot.cancel_turn();
    assert_eq!(slot.blocking, 0);
    assert!(!slot.unblock(epoch));
    // The done event that follows the cancellation is what publishes `idle`;
    // the stale completion must not publish anything in between.
    assert_eq!(slot.foreground, Foreground::RequiresAction);
}

#[test]
fn a_completion_from_a_finished_turn_cannot_publish_running() {
    let mut slot = running();
    let (epoch, _) = slot.block().expect("block");
    assert!(slot.end_turn());
    assert_eq!(slot.foreground, Foreground::Idle);
    assert!(!slot.unblock(epoch));
    assert_eq!(slot.foreground, Foreground::Idle);
    // A new turn cannot be resumed by the previous turn's completion either.
    assert!(slot.begin_turn());
    assert!(!slot.unblock(epoch));
    assert_eq!(slot.foreground, Foreground::Running);
}

#[test]
fn turn_end_publishes_idle_only_for_foreground_work() {
    let mut slot = Slot::new(Attach::Attached);
    assert!(!slot.end_turn());
    assert_eq!(slot.epoch, 1);
    let mut slot = running();
    let (_, _) = slot.block().expect("block");
    assert!(slot.end_turn());
    assert_eq!(slot.foreground, Foreground::Idle);
    assert_eq!(slot.blocking, 0);
}

#[test]
fn attach_hold_is_bounded_and_flushes_in_order() {
    use super::MAX_HELD_UPDATES;
    let mut slot = Slot::new(Attach::Attaching);
    for index in 0..MAX_HELD_UPDATES + 8 {
        slot.hold(serde_json::json!({"index": index}));
    }
    let held = slot.flush_held();
    assert_eq!(held.len(), MAX_HELD_UPDATES);
    assert_eq!(held[0]["index"], 0);
    assert!(slot.flush_held().is_empty());
}
