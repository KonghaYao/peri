use super::*;
use peri_acp_types::session_resources::{
    ControlCommand, ControlReceipt, ControlResolution, ControlState,
};

impl SessionResourcesImpl {
    pub(super) async fn read_session_control(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ControlState> {
        self.gate.load_control(id).await
    }

    pub(super) async fn write_session_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlReceipt> {
        self.gate.apply_control(command).await
    }

    pub(super) async fn reconcile_session_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlResolution> {
        self.gate.resolve_control(command).await
    }
}
