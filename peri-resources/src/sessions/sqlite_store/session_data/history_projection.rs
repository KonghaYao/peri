//! Rewind metadata reconciliation stays inside its deletion transaction.
use super::*;

pub(super) async fn reconcile_rewind(
    tx: &mut SqliteConnection,
    id: &ThreadId,
    clear_excluded: &[MessageId],
) -> SessionResourceResult<()> {
    let rows: Vec<(String, bool, bool, Option<String>)> = sqlx::query_as(
        "SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = ?1 ORDER BY rowid",
    ).bind(id.as_str()).fetch_all(&mut *tx).await.map_err(|e| map_sqlx(&e))?;
    let retained = rows
        .into_iter()
        .map(|(id, truncated, excluded, projection)| {
            let message =
                uuid::Uuid::parse_str(&id).map_err(|_| corrupt("invalid rewind message id"))?;
            let projection = projection
                .map(|p| serde_json::from_str(&p).map_err(|_| corrupt("invalid rewind projection")))
                .transpose()?;
            Ok((
                message.into(),
                MessageFlags {
                    truncated,
                    excluded,
                    projection,
                },
                false,
            ))
        })
        .collect::<SessionResourceResult<Vec<_>>>()?;
    let known: HashSet<_> = retained.iter().map(|(id, _, _)| *id).collect();
    if clear_excluded.iter().any(|id| !known.contains(id)) {
        return Err(invalid_input(
            "rewind flag target is not a history entry of this session",
        ));
    }
    for (message, flags) in
        peri_acp_types::store::history::rewind_projection_updates(&retained, clear_excluded)
    {
        sqlx::query("UPDATE messages SET truncated = ?1, excluded = ?2, projection = ?3 WHERE thread_id = ?4 AND message_id = ?5")
            .bind(flags.truncated).bind(flags.excluded).bind(projection_json(&flags)?)
            .bind(id.as_str()).bind(message.as_uuid().to_string()).execute(&mut *tx).await.map_err(|e| map_sqlx(&e))?;
    }
    Ok(())
}
