use super::*;
use peri_acp_types::session_resources::work::{
    PreparedWorkCommand, WorkQuery, WorkReceipt, WorkResolution, WorkSnapshot,
};

impl SessionResourcesImpl {
    pub(super) async fn read_work_command(
        &self,
        query: &peri_acp_types::session_resources::work::WorkCommandQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::OwnedWorkCommand>>
    {
        self.gate.ensure_recovery_permitted()?;
        self.gate.data().load_work_command(query).await
    }
    pub(super) async fn read_session_work(
        &self,
        query: &WorkQuery,
    ) -> SessionResourceResult<WorkSnapshot> {
        self.gate.load_work(query).await
    }
    pub(super) async fn write_work_mutation(
        &self,
        command: &PreparedWorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        self.gate.apply_work(command).await
    }
    pub(super) async fn reconcile_work_mutation(
        &self,
        command: &PreparedWorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        self.gate.resolve_work(command).await
    }
}
