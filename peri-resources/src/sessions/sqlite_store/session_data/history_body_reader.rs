//! One SQLite snapshot and incremental BLOB handle for a sequential inspection.

use super::blob_reader::open_blob;
use super::history::HISTORY_WINDOW_BODY_ROWID_SQL;
use super::history_window::require_committed_tail;
use super::inherited_read::map_inherited_blob_error;
use super::{MessageId, SessionResourceResult, SqliteConnection, SqliteSessionData, ThreadId};
use peri_acp_types::history_window::{HistoryCommittedTail, HistoryReadTicket, HistoryWindowError};
use peri_acp_types::session_resources::{HistoryBodyInspection, OwnHistoryBodyReader};

impl SqliteSessionData {
    pub(super) async fn own_body_rowid(
        connection: &mut SqliteConnection,
        id: &ThreadId,
        entry: MessageId,
        tail: &HistoryCommittedTail,
    ) -> SessionResourceResult<i64> {
        require_committed_tail(connection, id, tail).await?;
        let tail_position = i64::try_from(tail.position).map_err(|_| {
            super::corrupt("history window tail position exceeds the supported range")
        })?;
        let row: Option<i64> = sqlx::query_scalar(HISTORY_WINDOW_BODY_ROWID_SQL)
            .bind(id.as_str())
            .bind(entry.as_uuid().to_string())
            .bind(tail_position)
            .fetch_optional(connection)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        row.ok_or_else(|| {
            HistoryWindowError::anchor_unavailable(
                "history body entry is not present in this session",
            )
            .into()
        })
    }
}

#[async_trait::async_trait]
impl OwnHistoryBodyReader for SqliteSessionData {
    async fn inspect(
        &self,
        thread: &ThreadId,
        entry: MessageId,
        tail: &HistoryCommittedTail,
        _read_ticket: &HistoryReadTicket,
        inspection: HistoryBodyInspection,
    ) -> SessionResourceResult<bool> {
        let mut tx = self
            .database
            .pool
            .begin()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let rowid = Self::own_body_rowid(&mut tx, thread, entry, tail).await?;
        let (tx, visible) = inspect_own_row(tx, rowid, inspection).await?;
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(visible)
    }
}

/// Run a bounded inspection on the existing transaction, then return it to its owner.
pub(super) async fn inspect_own_row(
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    rowid: i64,
    inspection: HistoryBodyInspection,
) -> SessionResourceResult<(sqlx::Transaction<'static, sqlx::Sqlite>, bool)> {
    let runtime = tokio::runtime::Handle::current();
    // Dropping the request stops an already-running blocking task at its next
    // buffer refill. Neither the parser nor the SQLite read pins an async worker.
    let (alive, request) = tokio::sync::oneshot::channel::<()>();
    let (tx, result) = tokio::task::spawn_blocking(move || {
        let result = (|| {
            let mut locked = runtime
                .block_on(tx.lock_handle())
                .map_err(|error| super::map_sqlx(&error))?;
            let blob = open_blob(&mut locked, b"messages\0", b"content\0", rowid)
                .map_err(map_inherited_blob_error)?;
            let mut reader = blob.buffered_while(|| !alive.is_closed());
            inspection(&mut reader)
        })();
        (tx, result)
    })
    .await
    .map_err(|_| HistoryWindowError::ReadFailed {
        detail: "history body inspection stopped".to_owned(),
    })?;
    drop(request);
    let visible = result?;
    Ok((tx, visible))
}
