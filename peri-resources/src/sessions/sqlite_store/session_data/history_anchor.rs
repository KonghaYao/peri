//! Oldest displayable active own row for the initial pagination cursor.

use super::history_body_reader::inspect_own_row;
use super::{HistoryPageBounds, MessageId, SessionResourceResult, SqliteSessionData, ThreadId};
use peri_acp_types::history_page::message_id_of;
use peri_acp_types::history_page_visibility::history_body_displayable_to_client_buffered;

const CANDIDATES: i64 = 32;

impl SqliteSessionData {
    pub(super) async fn read_initial_history_anchor(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<(HistoryPageBounds, Option<MessageId>)> {
        let mut tx = self
            .database
            .pool
            .begin()
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        let (head, _) = Self::read_page_bounds(&mut tx, id, None).await?;
        let bounds = HistoryPageBounds::try_new(head.as_deref())?;
        let mut after = -1_i64;
        let mut anchor = None;
        'candidates: loop {
            // idx_messages_active bounds both the active rows and the rowid range.
            let rows: Vec<(i64, String)> = sqlx::query_as(
                "SELECT rowid, message_id FROM messages
                 WHERE thread_id = ?1 AND excluded = 0 AND rowid > ?2
                 ORDER BY rowid LIMIT ?3",
            )
            .bind(id.as_str())
            .bind(after)
            .bind(CANDIDATES)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| super::map_sqlx(&e))?;
            let last_batch = rows.len() < CANDIDATES as usize;
            for (rowid, message) in rows {
                after = rowid;
                anchor = Some(message_id_of(&message)?);
                let (next_tx, visible) = inspect_own_row(
                    tx,
                    rowid,
                    Box::new(|reader| {
                        history_body_displayable_to_client_buffered(reader).map_err(|error| {
                            if error.is_io() {
                                super::unavailable("history anchor body read failed")
                            } else {
                                super::corrupt("history anchor visibility is unreadable")
                            }
                        })
                    }),
                )
                .await?;
                tx = next_tx;
                if visible {
                    break 'candidates;
                }
            }
            if last_batch {
                break;
            }
        }
        tx.commit().await.map_err(|e| super::map_sqlx(&e))?;
        Ok((bounds, anchor))
    }
}
