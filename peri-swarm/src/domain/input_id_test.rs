use super::*;

#[test]
fn namespace_and_name_encoding_are_stable() {
    assert_eq!(
        IDEMPOTENCY_NAMESPACE,
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"peri-swarm.input-idempotency-key.v1"),
    );
    let id = LogicalId::parse("agent-01").unwrap();
    let first = input_id_for_key(&id, "submit-hello");
    assert_eq!(first, "4f126633-b225-54ae-83d3-25fcc0285b6e");
    assert_eq!(first, input_id_for_key(&id, "submit-hello"));
    assert_eq!(first.len(), 36);
    assert_eq!(Uuid::parse_str(&first).unwrap().get_version_num(), 5);
}

#[test]
fn derived_ids_are_scoped_to_the_session_and_preserve_key_bytes() {
    let first = LogicalId::parse("agent-01").unwrap();
    let second = LogicalId::parse("agent-02").unwrap();
    let plain = input_id_for_key(&first, "key");
    assert_ne!(plain, input_id_for_key(&second, "key"));
    assert_ne!(plain, input_id_for_key(&first, "Key"));
    assert_ne!(plain, input_id_for_key(&first, " key"));
    assert_ne!(plain, input_id_for_key(&first, "key "));
}
