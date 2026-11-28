use serde_json::json;

use super::*;

fn sample(used: u64, size: u64, input: u64, output: u64, read: u64, creation: u64) -> UsageSample {
    UsageSample {
        context_used: Some(used),
        context_size: Some(size),
        tokens: Some(TokenSample {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_creation_tokens: creation,
            model: Some("claude-test".to_owned()),
            request_id: Some("req-1".to_owned()),
        }),
        stop_reason: None,
    }
}

#[test]
fn observations_accumulate_tokens_and_keep_the_latest_context() {
    let mut usage = TurnUsage::default();
    usage.observe(&sample(100, 1000, 10, 2, 30, 5));
    usage.observe(&sample(250, 1000, 4, 3, 40, 6));

    assert_eq!(usage.input_tokens, Some(14));
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.cache_read_tokens, Some(70));
    assert_eq!(usage.cache_creation_tokens, Some(11));
    assert_eq!(
        usage.total_tokens,
        Some(100),
        "total sums the four counters"
    );
    assert_eq!(usage.context_used, Some(250), "context keeps the latest");
    assert_eq!(usage.context_size, Some(1000));
    assert_eq!(usage.calls, Some(2));
    assert_eq!(usage.model.as_deref(), Some("claude-test"));
    assert_eq!(usage.request_id.as_deref(), Some("req-1"));
}

#[test]
fn a_context_only_observation_never_writes_token_counters() {
    let mut usage = TurnUsage::default();
    usage.observe(&UsageSample {
        context_used: Some(1200),
        context_size: Some(200_000),
        tokens: None,
        stop_reason: None,
    });

    let value = usage.to_json().expect("context occupancy is a usage fact");
    assert_eq!(value["context_used"], json!(1200));
    assert_eq!(value["context_size"], json!(200_000));
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_creation_tokens",
        "total_tokens",
        "calls",
    ] {
        assert!(
            value.get(key).is_none(),
            "`{key}` must be absent instead of a fabricated zero: {value}"
        );
    }
}

#[test]
fn the_persisted_shape_is_stable_and_round_trips() {
    let mut usage = TurnUsage::default();
    usage.observe(&sample(950, 200_000, 30, 7, 60, 10));
    let value = usage.to_json().expect("usage");
    assert_eq!(
        value,
        json!({
            "input_tokens": 30,
            "output_tokens": 7,
            "cache_read_tokens": 60,
            "cache_creation_tokens": 10,
            "total_tokens": 107,
            "context_used": 950,
            "context_size": 200_000,
            "model": "claude-test",
            "request_id": "req-1",
            "calls": 1,
        }),
        "snake_case keys are the persisted contract"
    );
    assert_eq!(TurnUsage::from_json(&value), Some(usage));
}

#[test]
fn an_empty_accumulator_has_no_json() {
    assert!(TurnUsage::default().is_empty());
    assert_eq!(TurnUsage::default().to_json(), None);
    assert_eq!(TurnUsage::from_json(&json!("not-an-object")), None);
}

#[test]
fn totals_absorb_turns_in_chronological_order() {
    let mut first = TurnUsage::default();
    first.observe(&sample(100, 1000, 10, 2, 30, 5));
    let mut second = TurnUsage::default();
    second.observe(&sample(250, 1000, 4, 3, 40, 6));

    let mut totals = UsageTotals::default();
    totals.absorb(&first);
    totals.absorb(&second);
    assert_eq!(totals.turns, 2);
    assert_eq!(totals.calls, 2);
    assert_eq!(totals.input_tokens, Some(14));
    assert_eq!(totals.output_tokens, Some(5));
    assert_eq!(totals.cache_read_tokens, Some(70));
    assert_eq!(totals.cache_creation_tokens, Some(11));
    assert_eq!(totals.total_tokens, Some(100));
    assert_eq!(totals.context_used, Some(250), "latest turn wins");
    assert_eq!(totals.model.as_deref(), Some("claude-test"));
}

#[test]
fn totals_skip_empty_usage_and_keep_token_keys_absent() {
    let mut totals = UsageTotals::default();
    totals.absorb(&TurnUsage::default());
    assert!(totals.is_empty());

    let mut context_only = TurnUsage::default();
    context_only.observe(&UsageSample {
        context_used: Some(5),
        context_size: Some(10),
        tokens: None,
        stop_reason: None,
    });
    totals.absorb(&context_only);
    assert_eq!(totals.turns, 1);
    let value = serde_json::to_value(&totals).expect("totals");
    assert_eq!(value["turns"], json!(1));
    assert_eq!(value["calls"], json!(0));
    assert!(value.get("input_tokens").is_none(), "{value}");
}

#[test]
fn merging_counters_does_not_claim_a_latest_context_across_sessions() {
    let mut first = UsageTotals {
        input_tokens: Some(10),
        total_tokens: Some(10),
        turns: 1,
        calls: 1,
        context_used: Some(500),
        context_size: Some(1000),
        model: Some("a".to_owned()),
        ..UsageTotals::default()
    };
    let second = UsageTotals {
        input_tokens: Some(20),
        total_tokens: Some(20),
        turns: 2,
        calls: 3,
        ..UsageTotals::default()
    };
    first.merge_counters(&second);
    assert_eq!(first.input_tokens, Some(30));
    assert_eq!(first.total_tokens, Some(30));
    assert_eq!(first.turns, 3);
    assert_eq!(first.calls, 4);
    assert_eq!(first.context_used, Some(500));
    assert_eq!(first.model.as_deref(), Some("a"));
}

#[test]
fn metrics_window_seconds_widen_fractional_bounds() {
    let window = MetricsWindow {
        start_time: 1_700_000_000_500_000,
        end_time: 1_700_000_060_999_999,
        limit: 10,
    };
    assert_eq!(window.second_bounds(), (1_700_000_000, 1_700_000_060));
}

#[test]
fn a_metrics_session_prefers_the_observed_model() {
    let totals = UsageTotals {
        input_tokens: Some(3),
        output_tokens: Some(4),
        cache_read_tokens: Some(5),
        cache_creation_tokens: Some(6),
        total_tokens: Some(18),
        turns: 2,
        calls: 2,
        model: Some("observed".to_owned()),
        ..UsageTotals::default()
    };
    let row = metrics_session("agent-1", &totals, Some("configured".to_owned()), "t");
    assert_eq!(row.session_id, "agent-1");
    assert_eq!(row.agent_id, "root");
    assert_eq!(row.model.as_deref(), Some("observed"));
    assert_eq!(row.total_tokens, Some(18));
    assert_eq!(row.turns, 2);
    assert_eq!(row.last_active_at, "t");

    let configured = metrics_session("agent-1", &UsageTotals::default(), Some("x".into()), "t");
    assert_eq!(configured.model.as_deref(), Some("x"));
}
