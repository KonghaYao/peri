use super::*;

fn retained_keys(values: &HashMap<FoldKey, FoldState>, retention: &Retention) -> usize {
    values.keys().map(key_bytes).sum::<usize>()
        + retention.order.iter().map(key_bytes).sum::<usize>()
}

#[test]
fn many_manual_folds_keep_recent_choices_with_bounded_directory() {
    let mut values = HashMap::new();
    let mut retention = Retention::default();
    for index in 0..MAX_ENTRIES * 4 {
        retention.insert(
            &mut values,
            FoldKey::Tool(format!("tool-{index}")),
            FoldState::Expanded,
        );
    }
    assert_eq!(values.len(), MAX_ENTRIES);
    assert!(!values.contains_key(&FoldKey::Tool("tool-0".into())));
    let latest = FoldKey::Tool(format!("tool-{}", MAX_ENTRIES * 4 - 1));
    assert_eq!(values.get(&latest), Some(&FoldState::Expanded));
    assert_eq!(retention.order.len(), MAX_ENTRIES);
    assert!(retained_keys(&values, &retention) <= MAX_KEY_BYTES);
}

#[test]
fn updating_a_manual_fold_refreshes_its_retention() {
    let mut values = HashMap::new();
    let mut retention = Retention::default();
    let first = FoldKey::Reasoning("first".into());
    retention.insert(&mut values, first.clone(), FoldState::Expanded);
    for index in 1..MAX_ENTRIES {
        retention.insert(
            &mut values,
            FoldKey::Tool(format!("tool-{index}")),
            FoldState::Expanded,
        );
    }
    retention.insert(&mut values, first.clone(), FoldState::Collapsed);
    retention.insert(
        &mut values,
        FoldKey::Tool("new".into()),
        FoldState::Expanded,
    );
    assert_eq!(values.get(&first), Some(&FoldState::Collapsed));
    assert!(!values.contains_key(&FoldKey::Tool("tool-1".into())));
    assert_eq!(retention.order.len(), MAX_ENTRIES);
}

#[test]
fn growing_group_keys_are_bounded_by_bytes_before_entry_limit() {
    let mut values = HashMap::new();
    let mut retention = Retention::default();
    for count in 1..300 {
        let key = FoldKey::Group((0..count).map(|id| format!("tool-{id:08}")).collect());
        retention.insert(&mut values, key, FoldState::Expanded);
        assert!(retained_keys(&values, &retention) <= MAX_KEY_BYTES);
    }
    assert!(values.len() < 299);
    assert_eq!(values.len(), retention.order.len());
}

#[test]
fn oversized_group_does_not_evict_existing_manual_choices() {
    let mut values = HashMap::new();
    let mut retention = Retention::default();
    let keep = FoldKey::Tool("keep".into());
    retention.insert(&mut values, keep.clone(), FoldState::Expanded);
    let mut large_id = String::with_capacity(MAX_KEY_BYTES);
    large_id.push('x');
    let oversized = FoldKey::Group(vec![large_id]);
    retention.insert(&mut values, oversized, FoldState::Expanded);
    assert_eq!(values.len(), 1);
    assert_eq!(values.get(&keep), Some(&FoldState::Expanded));
    assert_eq!(retention.order.len(), 1);
}

#[test]
fn removed_history_keys_do_not_accumulate_in_retention_metadata() {
    let mut values = HashMap::new();
    let mut retention = Retention::default();
    for index in 0..MAX_ENTRIES * 2 {
        values.clear();
        retention.insert(
            &mut values,
            FoldKey::Interaction(format!("request-{index}")),
            FoldState::Expanded,
        );
        assert_eq!(retention.order.len(), 1);
    }
}
