//! 远端会话的机器与工作区归属读取。

use crate::sessions::remote::sql::Value;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::thread::ThreadId;

use super::super::session_codec;
use super::super::sql::{self, StatementSpec};
use super::RemoteSessionData;

impl RemoteSessionData {
    pub(super) async fn read_machine_id_of(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                "SELECT w.machine_id FROM threads t JOIN workspaces w ON w.id = t.workspace_id WHERE t.id = $1",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        Ok(row.and_then(|values| sql::text_at(&values, 0).map(str::to_owned)))
    }

    pub(super) async fn read_workspace_id_of(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::workspace::WorkspaceId>> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                "SELECT workspace_id FROM threads WHERE id = $1",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        row.map(|row| {
            sql::text_at(&row, 0)
                .ok_or_else(|| session_codec::corrupt("invalid remote workspace id"))?
                .parse()
                .map_err(|_| session_codec::corrupt("invalid remote workspace id"))
        })
        .transpose()
    }
}
