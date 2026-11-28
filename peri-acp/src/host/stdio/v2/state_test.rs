use peri_acp_types::history_page::{HistoryCursor, HistoryCursorEnvelope};
use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;
use serde_json::json;

use super::{Attach, Connection, Foreground, Slot, V1Admission};

/// A slot with the identity `Connection::insert_slot` would give it.
fn new_slot(attach: Attach) -> Slot {
    let mut slot = Slot::new(attach);
    slot.incarnation = 1;
    slot
}

/// A cursor the host's `classify` accepts as a history page position.
fn cursor_for(thread: &str) -> String {
    HistoryCursorEnvelope::new(
        0,
        format!("{thread}-snapshot"),
        HistoryCursor::new(
            ThreadId::from(thread.to_string()),
            MessageId::new(),
            format!("{thread}-head"),
        ),
    )
    .encode()
}

fn running() -> Slot {
    let mut slot = new_slot(Attach::Attached);
    assert!(slot.begin_turn());
    slot
}

#[test]
fn turn_start_publishes_running_once_and_bumps_the_epoch() {
    let mut slot = new_slot(Attach::Attached);
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
        !slot.unblock(slot.incarnation, epoch),
        "one of two completions must not resume the turn"
    );
    assert_eq!(slot.foreground, Foreground::RequiresAction);
    assert!(slot.unblock(slot.incarnation, epoch));
    assert_eq!(slot.foreground, Foreground::Running);
    assert_eq!(slot.blocking, 0);
}

#[test]
fn a_completion_from_a_cancelled_turn_cannot_publish_running() {
    let mut slot = running();
    let (epoch, _) = slot.block().expect("block");
    slot.cancel_turn();
    assert_eq!(slot.blocking, 0);
    assert!(!slot.unblock(slot.incarnation, epoch));
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
    assert!(!slot.unblock(slot.incarnation, epoch));
    assert_eq!(slot.foreground, Foreground::Idle);
    // A new turn cannot be resumed by the previous turn's completion either.
    assert!(slot.begin_turn());
    assert!(!slot.unblock(slot.incarnation, epoch));
    assert_eq!(slot.foreground, Foreground::Running);
}

#[test]
fn turn_end_publishes_idle_only_for_foreground_work() {
    let mut slot = new_slot(Attach::Attached);
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
    let mut slot = new_slot(Attach::Attaching);
    for index in 0..MAX_HELD_UPDATES + 8 {
        slot.hold(serde_json::json!({"index": index}));
    }
    let held = slot.flush_held();
    assert_eq!(held.len(), MAX_HELD_UPDATES);
    assert_eq!(held[0]["index"], 0);
    assert!(slot.flush_held().is_empty());
}

#[test]
fn read_only_history_loads_do_not_take_the_v1_setup_gate() {
    let mut connection = Connection::default();
    connection.commit_v1_setup("A");

    // The host answers these from storage before reserving or binding a
    // session, so a v1 connection bound to A may still browse B's history.
    let page = json!({"sessionId": "B", "cursor": cursor_for("B")});
    assert!(matches!(
        connection.admit_v1_setup("session/load", &page),
        V1Admission::Pass
    ));
    let window = json!({
        "sessionId": "B",
        "history": {"kind": "open", "direction": "around"}
    });
    assert!(matches!(
        connection.admit_v1_setup("session/load", &window),
        V1Admission::Pass
    ));

    // The binding is untouched: another setup is still refused.
    assert!(matches!(
        connection.admit_v1_setup("session/new", &json!({"cwd": "/workspace"})),
        V1Admission::Refused(_)
    ));
}

#[test]
fn an_initial_load_still_counts_as_a_v1_setup() {
    let mut connection = Connection::default();

    // No cursor and no history: the load assembles the session, so it takes the
    // gate and is recorded to bind on success.
    let load = json!({"sessionId": "B"});
    assert!(matches!(
        connection.admit_v1_setup("session/load", &load),
        V1Admission::Tracked(_)
    ));
    // While that setup is in flight nothing else may start.
    assert!(matches!(
        connection.admit_v1_setup("session/new", &json!({"cwd": "/workspace"})),
        V1Admission::Refused(_)
    ));

    // A malformed page is not a read-only read: it goes to the host, which
    // answers with its own error shape.
    let mut connection = Connection::default();
    let malformed = json!({"sessionId": "B", "cursor": "not-a-cursor"});
    assert!(matches!(
        connection.admit_v1_setup("session/load", &malformed),
        V1Admission::Tracked(_)
    ));
}
