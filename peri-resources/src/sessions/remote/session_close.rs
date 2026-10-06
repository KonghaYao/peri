use peri_acp_types::session_resources::{
    CloseSettlement, ControlAction, ControlDecision, ControlStatus, SessionResourceError,
    SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use turso_serverless::Value;

use super::super::mutation::RemoteStore;
use super::super::sql::{int_at, text_at, StatementSpec};
use super::RemoteSessionData;
use crate::sessions::{control, failure::corrupt};

impl RemoteSessionData {
    pub(in crate::sessions::remote) async fn change_close_status(
        &self,
        id: &ThreadId,
        action: ControlAction,
    ) -> SessionResourceResult<()> {
        let current = self.read_control(id).await?;
        if action == ControlAction::Close && current.status == ControlStatus::Closing {
            return Ok(());
        }
        let command = control::close_command(id, &current, action)?;
        let receipt = self.write_control(&command).await?;
        if receipt.decision != ControlDecision::Accepted {
            return Err(SessionResourceError::conflict(
                "session close control transition was rejected",
            ));
        }
        Ok(())
    }

    pub(super) async fn finish_session_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.change_close_status(id, ControlAction::FinishClose)
            .await
    }
}

impl RemoteStore {
    pub(in crate::sessions::remote) async fn close_settlement(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<CloseSettlement> {
        let row = self.fetch_row(&StatementSpec::new(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1),state_json FROM (SELECT 1) LEFT JOIN session_control_state ON session_id=?1",
            vec![Value::Text(id.clone())],
        )).await?.ok_or_else(||corrupt("session close settlement is missing"))?;
        let exists = int_at(&row, 0).ok_or_else(|| corrupt("invalid close settlement reply"))?;
        let json = match row.get(1) {
            Some(Value::Null) => None,
            Some(Value::Text(_)) => text_at(&row, 1),
            _ => return Err(corrupt("invalid close control state")),
        };
        if exists == 0 && json.is_none() {
            return Ok(CloseSettlement::Unknown);
        }
        match control::state(json)?.status {
            ControlStatus::Closing => Ok(CloseSettlement::Pending),
            ControlStatus::Closed => Ok(CloseSettlement::Finished),
            ControlStatus::Active | ControlStatus::Paused if exists != 0 => {
                Ok(CloseSettlement::Finished)
            }
            ControlStatus::Active | ControlStatus::Paused => Ok(CloseSettlement::Unknown),
        }
    }
}
