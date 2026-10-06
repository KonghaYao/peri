use peri_acp_types::session_resources::{
    work::{WorkInspection, WorkPage, WorkQuery, WorkResolution, WorkSelector},
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult, SessionResources,
};

pub(super) async fn resolve_pending(
    resources: &dyn SessionResources,
    query: &WorkQuery,
) -> SessionResourceResult<WorkInspection> {
    let mut pending = WorkQuery::new(&query.session_id, WorkSelector::PendingCommands);
    loop {
        let inspection = resources.inspect_work(&pending).await?;
        let WorkPage::Commands(commands) = inspection.page else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "expected pending command page".into(),
                },
            ));
        };
        for owned in commands {
            match resources.resolve_work_mutation(&owned.command).await {
                Ok(WorkResolution::Unknown) => return resources.inspect_work(query).await,
                Err(error) if error.is_persistence_uncertain() => {
                    return resources.inspect_work(query).await;
                }
                Err(error) => return Err(error),
                Ok(_) => {}
            }
        }
        let Some(cursor) = inspection.next_cursor else {
            break;
        };
        pending.cursor = Some(cursor);
    }
    resources.inspect_work(query).await
}
