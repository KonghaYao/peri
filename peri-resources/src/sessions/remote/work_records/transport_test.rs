use super::*;

#[test]
fn workrecords_transport_preserves_parameter_types() {
    let parameters = vec![
        SqlParam::Text("123".into()),
        SqlParam::Integer(123),
        SqlParam::Null,
        SqlParam::Blob(vec![0, 255]),
    ];
    let converted = specifications(vec![SqlStatement::new(
        "SELECT ?1,?2,?3,?4",
        parameters.clone(),
    )])
    .unwrap();
    assert_eq!(
        rows(vec![vec![converted[0].params.clone()]]).unwrap(),
        vec![vec![parameters]]
    );
}

#[test]
fn workrecords_transport_rejects_whole_oversized_transaction() {
    let statements = vec![StatementSpec::bare("SELECT 1"); MAX_STATEMENTS];
    assert!(validate_budget(&statements)
        .unwrap_err()
        .to_string()
        .contains("statement budget"));
    let statement = StatementSpec::new(
        "SELECT ?1",
        vec![Value::Blob(vec![0; MAX_TRANSACTION_BYTES / 2])],
    );
    assert!(validate_budget(&[statement])
        .unwrap_err()
        .to_string()
        .contains("byte budget"));
    let statement = StatementSpec::new("SELECT ?1", vec![Value::Null; MAX_PARAMETERS + 1]);
    assert!(validate_budget(&[statement])
        .unwrap_err()
        .to_string()
        .contains("parameter budget"));
}
