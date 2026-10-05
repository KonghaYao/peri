//! Durable close intent settlement without execution ownership.

use peri_acp_types::session_resources::{
    CloseSettlement, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use turso_serverless::Value;

use super::super::mutation::RemoteStore;
use super::super::sql::{int_at, StatementSpec};
use super::{not_found, RemoteSessionData};

impl RemoteSessionData {
    pub(super) async fn finish_session_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        if !self.exists(id).await? {
            return Err(not_found());
        }
        if !self.read_close_intent(id).await? {
            return Err(SessionResourceError::conflict(
                "session close intent is missing",
            ));
        }
        self.commit_effects(
            "finish_close",
            &[id.clone()],
            vec![StatementSpec::new(
                "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
                    (SELECT 1 FROM threads t JOIN session_close_intents c ON c.thread_id = t.id WHERE t.id = ?1)",
                vec![Value::Text(id.clone())],
            ), StatementSpec::new(
                "DELETE FROM session_close_intents WHERE thread_id = ?1",
                vec![Value::Text(id.clone())],
            )],
            id,
        )
        .await
        .map(|_| ())
    }
}

impl RemoteStore {
    pub(in crate::sessions::remote) async fn close_settlement(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<CloseSettlement> {
        let row = self
            .fetch_row(&StatementSpec::new(
                "SELECT EXISTS (SELECT 1 FROM threads WHERE id = ?1),
                    EXISTS (SELECT 1 FROM session_close_intents WHERE thread_id = ?1)",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        match row.as_deref().map(|row| (int_at(row, 0), int_at(row, 1))) {
            Some((Some(0), _)) => Ok(CloseSettlement::Unknown),
            Some((Some(1), Some(1))) => Ok(CloseSettlement::Pending),
            Some((Some(1), Some(0))) => Ok(CloseSettlement::Finished),
            _ => Err(SessionResourceError::new(
                SessionResourceErrorKind::Internal {
                    detail: "invalid close settlement reply".to_owned(),
                },
            )),
        }
    }
}
