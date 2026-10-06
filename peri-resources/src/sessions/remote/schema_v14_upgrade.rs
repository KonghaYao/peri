use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

use super::{mutation::RemoteStore, schema::StoreSnapshot};

pub(super) async fn upgrade(
    _store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    tracing::error!(
        schema_version = snapshot.schema_version,
        "legacy store requires explicit stopped-writer offline work migration"
    );
    Err(SessionResourceError::new(
        SessionResourceErrorKind::Unsupported,
    ))
}
