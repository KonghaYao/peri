use peri_acp_types::session_resources::{
    work::{PreparedWorkCommand, WorkQuery, WorkResolution, WorkSnapshot},
    SessionResourceResult, SessionResources,
};

pub(super) async fn resolve_pending(
    resources: &dyn SessionResources,
    query: &WorkQuery,
) -> SessionResourceResult<WorkSnapshot> {
    let snapshot = resources.load_session_work(query).await?;
    for command in &snapshot.pending_commands {
        let command = PreparedWorkCommand::try_new(command.clone())?;
        match resources.resolve_work_mutation(&command).await {
            Ok(WorkResolution::Unknown) => return Ok(snapshot),
            Err(error) if error.is_persistence_uncertain() => {
                return Ok(snapshot);
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    resources.load_session_work(query).await
}
