use std::time::Duration;

use super::*;

#[test]
fn list_interval_grows_with_the_loaded_window() {
    for loaded in [0, 1, 50, 100] {
        assert_eq!(list_interval(loaded), Duration::from_secs(3), "{loaded}");
    }
    for loaded in [101, 200, 300] {
        assert_eq!(list_interval(loaded), Duration::from_secs(5), "{loaded}");
    }
    for loaded in [301, 400, 500] {
        assert_eq!(list_interval(loaded), Duration::from_secs(10), "{loaded}");
    }
}

#[test]
fn detail_interval_follows_open_work() {
    assert_eq!(detail_interval("in_progress"), Duration::from_secs(2));
    assert_eq!(detail_interval("requires_action"), Duration::from_secs(2));
    for status in ["idle", "failed", "unknown"] {
        assert_eq!(detail_interval(status), Duration::from_secs(5), "{status}");
    }
}

#[test]
fn backoff_follows_the_documented_sequence_and_cap() {
    assert_eq!(backoff_delay(0), None);
    let expected = [(1, 3), (2, 6), (3, 12), (4, 24), (5, 30), (9, 30)];
    for (failures, seconds) in expected {
        assert_eq!(
            backoff_delay(failures),
            Some(Duration::from_secs(seconds)),
            "failures={failures}"
        );
    }
}

#[test]
fn schedule_delay_never_polls_faster_than_the_base() {
    assert_eq!(
        schedule_delay(Duration::from_secs(3), 0),
        Duration::from_secs(3)
    );
    assert_eq!(
        schedule_delay(Duration::from_secs(3), 2),
        Duration::from_secs(6)
    );
    assert_eq!(
        schedule_delay(Duration::from_secs(10), 1),
        Duration::from_secs(10)
    );
    assert_eq!(
        schedule_delay(Duration::from_secs(10), 4),
        Duration::from_secs(24)
    );
    assert_eq!(
        schedule_delay(Duration::from_secs(10), 5),
        Duration::from_secs(30)
    );
}

#[test]
fn one_same_kind_request_runs_at_a_time() {
    let mut state = PollState::new();
    let first = state.begin().expect("first request");
    assert!(state.is_in_flight());
    assert!(state.begin().is_none());
    assert!(state.complete(first));
    assert!(!state.is_in_flight());
    assert!(state.begin().is_some());
}

#[test]
fn invalidation_discards_an_old_response_without_overlapping() {
    let mut state = PollState::new();
    let old = state.begin().expect("old request");
    let current = state.invalidate();
    assert_ne!(old, current);
    // The old request still owns the slot.
    assert!(state.begin().is_none());
    assert!(!state.complete(old));
    assert!(!state.is_in_flight());

    let next = state.begin().expect("current request");
    assert_eq!(next, current);
    assert!(state.complete(next));
}

#[test]
fn stale_failure_does_not_start_a_backoff_streak() {
    let mut state = PollState::new();
    let old = state.begin().expect("old request");
    state.invalidate();
    assert_eq!(state.fail(old), None);
    assert_eq!(state.failures(), 0);
    assert!(!state.is_in_flight());
    assert_eq!(state.begin(), Some(state.generation()));
}

#[test]
fn failures_grow_the_delay_and_success_resets_it() {
    let mut state = PollState::new();
    let expected = [
        Duration::from_secs(3),
        Duration::from_secs(6),
        Duration::from_secs(12),
        Duration::from_secs(24),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ];
    for delay in expected {
        let generation = state.begin().expect("request");
        assert_eq!(state.fail(generation), Some(delay));
    }
    assert_eq!(state.failures(), 6);
    assert_eq!(state.delay(Duration::from_secs(3)), Duration::from_secs(30));

    let generation = state.begin().expect("successful request");
    assert!(state.complete(generation));
    assert_eq!(state.failures(), 0);
    assert_eq!(state.delay(Duration::from_secs(3)), Duration::from_secs(3));
}

#[test]
fn pause_blocks_requests_and_resume_issues_a_new_generation() {
    let mut state = PollState::new();
    let before = state.generation();
    state.pause();
    assert!(state.is_paused());
    assert!(state.begin().is_none());
    assert_ne!(state.generation(), before);

    state.resume();
    assert!(!state.is_paused());
    let generation = state.begin().expect("request after resume");
    assert_eq!(generation, state.generation());
}

#[test]
fn pause_discards_the_response_that_was_already_running() {
    let mut state = PollState::new();
    let old = state.begin().expect("request before pause");
    state.pause();
    // The slot stays owned until the request finishes.
    assert!(state.begin().is_none());
    assert!(!state.complete(old));
    assert!(!state.is_in_flight());

    state.resume();
    assert!(state.begin().is_some());
}

#[test]
fn explicit_reset_clears_backoff_without_touching_pause() {
    let mut state = PollState::new();
    let generation = state.begin().expect("request");
    assert!(state.fail(generation).is_some());
    assert_eq!(state.failures(), 1);

    state.reset_backoff();
    assert_eq!(state.failures(), 0);
    assert!(!state.is_paused());
}
