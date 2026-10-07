use super::*;
use peri_acp_types::session_resources::SessionResourceErrorKind;

#[test]
fn deduplicated_reference_validates_actual_scope_metadata_and_bytes() {
    let write = EvidenceWrite {
        session_id: "session".into(),
        storage_scope: "session".into(),
        payload_id: "requested".into(),
        encoding: 1,
        bytes: b"{}".to_vec(),
    };
    let expected = write.reference().unwrap();
    let row = vec![
        SqlParam::Text("session".into()),
        SqlParam::Text("canonical".into()),
        SqlParam::Text("json".into()),
        SqlParam::Integer(1),
        SqlParam::Integer(2),
        SqlParam::Text(expected.sha256),
        SqlParam::Blob(write.bytes.clone()),
    ];
    let actual = prepared_evidence_reference(&write, &vec![row.clone()]).unwrap();
    assert_eq!(actual.payload_id, "canonical");
    assert_eq!(actual.storage_scope, write.storage_scope);
    for (column, value) in [
        (0, SqlParam::Text("wrong-scope".into())),
        (2, SqlParam::Text("wrong-codec".into())),
        (3, SqlParam::Integer(2)),
        (4, SqlParam::Integer(3)),
        (5, SqlParam::Text("wrong-digest".into())),
        (6, SqlParam::Blob(b"[]".to_vec())),
    ] {
        let mut corrupted = row.clone();
        corrupted[column] = value;
        let error = prepared_evidence_reference(&write, &vec![corrupted]).unwrap_err();
        if column == 6 {
            assert!(matches!(
                error.kind(),
                SessionResourceErrorKind::InvalidInput { .. }
            ));
        } else {
            assert!(matches!(
                error.kind(),
                SessionResourceErrorKind::Corrupt { .. }
            ));
        }
    }
}
