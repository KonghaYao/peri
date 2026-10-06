use super::*;
use peri_acp_types::session_resources::work::{
    WorkCommand, WorkInspection, WorkQuery, WorkReceipt, WorkResolution,
};

impl SessionResourcesImpl {
    pub(super) async fn read_session_work(
        &self,
        query: &WorkQuery,
    ) -> SessionResourceResult<WorkInspection> {
        self.gate.load_work(query).await
    }
    pub(super) async fn write_work_mutation(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        self.gate.apply_work(command).await
    }
    pub(super) async fn reconcile_work_mutation(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        self.gate.resolve_work(command).await
    }
}
