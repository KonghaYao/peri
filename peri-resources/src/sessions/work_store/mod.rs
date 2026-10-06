pub(crate) mod migration;
pub(crate) mod payload;
pub(crate) mod schema;
mod selection;
pub(crate) use selection::{decode_facts, decode_inspection, inspection_plan, read_set};
mod plan;
pub(crate) use plan::sql_plan;
mod response;
pub(crate) use response::{response_validation_plan, validate_response_transition};
mod lifecycle;
pub(crate) use lifecycle::{creation_guard, history_guard, tombstone_plan};

use peri_acp_types::session_resources::work::{
    EvidenceQuery, EvidenceRecord, EvidenceWrite, PayloadRef,
};
use peri_acp_types::session_resources::SessionResourceResult;

pub(crate) fn evidence_plan(query: &EvidenceQuery) -> SessionResourceResult<Vec<SqlStatement>> {
    query.reference.validate()?;
    if query.session_id.is_empty() {
        return Err(crate::sessions::failure::corrupt(
            "missing evidence session",
        ));
    }
    Ok(vec![SqlStatement::new("SELECT p.version,p.byte_length,p.sha256,p.bytes FROM session_payloads p WHERE p.storage_scope=?1 AND p.payload_id=?2 AND EXISTS(SELECT 1 FROM threads WHERE id=?3 UNION ALL SELECT 1 FROM session_control_state WHERE session_id=?3) AND (?1=?3 OR EXISTS(SELECT 1 FROM messages WHERE thread_id=?3 AND content_ref=?4) OR EXISTS(SELECT 1 FROM session_deliveries WHERE session_id=?3 AND json_extract(record_json,'$.publication.event.content.content')=?4))",vec![SqlParam::Text(query.reference.storage_scope.clone()),SqlParam::Text(query.reference.payload_id.clone()),SqlParam::Text(query.session_id.clone()),SqlParam::Text(crate::sessions::work::encode(&query.reference)?)])])
}

pub(crate) fn decode_evidence(
    query: &EvidenceQuery,
    rows: &[SqlRows],
) -> SessionResourceResult<EvidenceRecord> {
    payload::decode_evidence(
        &query.reference,
        rows.first()
            .ok_or_else(|| crate::sessions::failure::corrupt("missing evidence result"))?,
    )
}

pub(crate) fn prepare_evidence_plan(
    write: &EvidenceWrite,
) -> SessionResourceResult<(PayloadRef, Vec<SqlStatement>)> {
    if write.storage_scope != write.session_id {
        return Err(crate::sessions::failure::corrupt(
            "evidence writes must use their session storage scope",
        ));
    }
    let (reference, mut statements) = payload::prepare_write(write)?;
    statements.insert(0,SqlStatement::new("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM threads WHERE id=?1)",vec![SqlParam::Text(write.session_id.clone())]));
    Ok((reference, statements))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SqlParam {
    Text(String),
    Integer(i64),
    Null,
    Blob(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SqlStatement {
    pub sql: &'static str,
    pub params: Vec<SqlParam>,
    pub expected_rows: Option<u64>,
}

impl SqlStatement {
    pub fn new(sql: &'static str, params: Vec<SqlParam>) -> Self {
        Self {
            sql,
            params,
            expected_rows: None,
        }
    }

    pub fn bare(sql: &'static str) -> Self {
        Self::new(sql, Vec::new())
    }

    pub fn checked(sql: &'static str, params: Vec<SqlParam>, expected_rows: u64) -> Self {
        Self {
            sql,
            params,
            expected_rows: Some(expected_rows),
        }
    }
}

pub(crate) type SqlRows = Vec<Vec<SqlParam>>;
