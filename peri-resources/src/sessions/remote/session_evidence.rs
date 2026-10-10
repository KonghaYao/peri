//! Reads immutable execution evidence from the remote store.

use super::session_data::RemoteSessionData;
use super::sql::StatementSpec;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::thread::ThreadId;
use turso_serverless::Value;

impl RemoteSessionData {
    pub(super) async fn read_binding_discovery_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        if self.schema_version <= 11 {
            return Ok(None);
        }
        let row = self
            .store()
            .await?
            .fetch_row(&StatementSpec::new(
                "SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        match row.as_deref().and_then(|row| row.first()) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Text(snapshot)) => Ok(Some(snapshot.clone())),
            _ => Err(super::session_codec::corrupt(
                "invalid remote binding evidence",
            )),
        }
    }
}
