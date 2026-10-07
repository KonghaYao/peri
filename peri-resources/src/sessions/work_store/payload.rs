use peri_acp_types::session_resources::work::{EvidenceRecord, EvidenceWrite, PayloadRef};
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use sha2::{Digest, Sha256};

use super::{SqlParam, SqlRows, SqlStatement};
use crate::sessions::failure::corrupt;

pub(crate) const READ_PAYLOAD: &str = "SELECT version,byte_length,sha256,bytes FROM session_payloads WHERE storage_scope=?1 AND payload_id=?2";
const INSERT_PAYLOAD: &str = "INSERT OR IGNORE INTO session_payloads(storage_scope,payload_id,kind,codec,version,byte_length,sha256,bytes,retention_class) VALUES (?1,?2,?3,'json',?4,?5,?6,?7,'evidence')";
const GUARD_PAYLOAD: &str = "INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_payloads WHERE storage_scope=?1 AND payload_id=?2 AND version=?3 AND byte_length=?4 AND sha256=?5 AND bytes=?6)";
const GUARD_EVIDENCE: &str = "INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_payloads WHERE storage_scope=?1 AND payload_id=?2 AND (codec<>'json' OR version<>?3 OR byte_length<>?4 OR sha256<>?5 OR bytes<>?6)) OR NOT EXISTS(SELECT 1 FROM session_payloads WHERE storage_scope=?1 AND codec='json' AND version=?3 AND byte_length=?4 AND sha256=?5 AND bytes=?6 AND (payload_id=?2 OR kind='evidence'))";
pub(crate) const GUARD_REFERENCE: &str = "INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_payloads WHERE storage_scope=?1 AND payload_id=?2 AND version=?3 AND byte_length=?4 AND sha256=?5)";

pub(crate) fn integer(value: u64) -> SessionResourceResult<SqlParam> {
    i64::try_from(value)
        .map(SqlParam::Integer)
        .map_err(|_| corrupt("work integer exceeds SQLite range"))
}

pub(crate) fn reference_params(reference: &PayloadRef) -> SessionResourceResult<Vec<SqlParam>> {
    reference.validate()?;
    Ok(vec![
        SqlParam::Text(reference.storage_scope.clone()),
        SqlParam::Text(reference.payload_id.clone()),
        integer(reference.encoding.into())?,
        integer(reference.byte_length)?,
        SqlParam::Text(reference.sha256.clone()),
    ])
}

pub(crate) fn guard(reference: &PayloadRef) -> SessionResourceResult<SqlStatement> {
    Ok(SqlStatement::new(
        GUARD_REFERENCE,
        reference_params(reference)?,
    ))
}

pub(crate) fn prepare_write(
    write: &EvidenceWrite,
) -> SessionResourceResult<(PayloadRef, Vec<SqlStatement>)> {
    let reference = write.reference()?;
    let mut params = reference_params(&reference)?;
    params.insert(2, SqlParam::Text("evidence".into()));
    params.push(SqlParam::Blob(write.bytes.clone()));
    let mut guard_params = reference_params(&reference)?;
    guard_params.push(SqlParam::Blob(write.bytes.clone()));
    Ok((
        reference,
        vec![
            SqlStatement::new(INSERT_PAYLOAD, params),
            SqlStatement::new(GUARD_EVIDENCE, guard_params),
        ],
    ))
}

pub(crate) fn prepare_bytes(
    scope: &str,
    kind: &str,
    bytes: &[u8],
) -> SessionResourceResult<(PayloadRef, Vec<SqlStatement>)> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let write = EvidenceWrite {
        session_id: scope.into(),
        storage_scope: scope.into(),
        payload_id: format!("{kind}:{digest}"),
        encoding: 1,
        bytes: bytes.to_vec(),
    };
    let (reference, mut statements) = prepare_write(&write)?;
    statements[0].params[2] = SqlParam::Text(kind.into());
    statements[1].sql = GUARD_PAYLOAD;
    Ok((reference, statements))
}

pub(crate) fn prepared_evidence_plan(write: &EvidenceWrite) -> SessionResourceResult<SqlStatement> {
    let reference = write.reference()?;
    Ok(SqlStatement::new("SELECT storage_scope,payload_id,codec,version,byte_length,sha256,bytes FROM session_payloads WHERE storage_scope=?1 AND version=?3 AND byte_length=?4 AND sha256=?5 AND (payload_id=?2 OR (kind='evidence' AND codec='json')) ORDER BY payload_id=?2 DESC LIMIT 1", reference_params(&reference)?))
}

pub(crate) fn prepared_evidence_reference(
    write: &EvidenceWrite,
    rows: &SqlRows,
) -> SessionResourceResult<PayloadRef> {
    let row = rows
        .first()
        .ok_or_else(|| corrupt("prepared immutable evidence is missing"))?;
    let (Some(SqlParam::Text(scope)), Some(SqlParam::Text(identity))) = (row.first(), row.get(1))
    else {
        return Err(corrupt("prepared immutable evidence identity is invalid"));
    };
    if scope != &write.storage_scope {
        return Err(corrupt("prepared immutable evidence scope conflicts"));
    }
    if row.get(2) != Some(&SqlParam::Text("json".into())) {
        return Err(corrupt("prepared immutable evidence codec conflicts"));
    }
    let mut reference = write.reference()?;
    reference.payload_id = identity.clone();
    let evidence = decode_evidence(&reference, &vec![row.iter().skip(3).cloned().collect()])?;
    if evidence.bytes != write.bytes {
        return Err(corrupt("prepared immutable evidence bytes conflict"));
    }
    Ok(reference)
}

pub(crate) fn prepare(
    value: &PersistedPayload,
    scope: &str,
) -> SessionResourceResult<(PayloadRef, Vec<SqlStatement>)> {
    let encoded = serialize_persisted_payload(value)
        .map_err(|_| corrupt("canonical payload serialization failed"))?;
    prepare_bytes(scope, "message", encoded.as_bytes())
}

pub(crate) fn read_statement(reference: &PayloadRef) -> SessionResourceResult<SqlStatement> {
    reference.validate()?;
    Ok(SqlStatement::new(
        READ_PAYLOAD,
        vec![
            SqlParam::Text(reference.storage_scope.clone()),
            SqlParam::Text(reference.payload_id.clone()),
        ],
    ))
}

pub(crate) fn decode_evidence(
    reference: &PayloadRef,
    rows: &SqlRows,
) -> SessionResourceResult<EvidenceRecord> {
    let row = rows
        .first()
        .ok_or_else(|| corrupt("immutable payload is missing"))?;
    let [SqlParam::Integer(version), SqlParam::Integer(length), SqlParam::Text(digest), SqlParam::Blob(bytes)] =
        row.as_slice()
    else {
        return Err(corrupt("immutable payload row is invalid"));
    };
    if *version != i64::from(reference.encoding)
        || u64::try_from(*length).ok() != Some(reference.byte_length)
        || digest != &reference.sha256
    {
        return Err(corrupt("immutable payload metadata mismatch"));
    }
    let evidence = EvidenceRecord {
        reference: reference.clone(),
        bytes: bytes.clone(),
    };
    evidence.validate()?;
    Ok(evidence)
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
