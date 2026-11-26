//! Open and validate the PostgreSQL schema before exposing a session handle.

use super::super::mutation::{RemoteStore, StoreAccess};
use super::super::schema::{self, StoreId, StoreIdentityOutcome, StoreIdentityRead};
use super::super::sql::{self, StatementSpec, Value};
use super::StoreInitialization;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

/// Resolve the execution machine only after the remote store's identity has
/// been read. A read-only Native open must not create a local machine ID.
pub(super) async fn resolve_open_machine_id(
    supplied: Option<String>,
    access: StoreAccess,
) -> SessionResourceResult<String> {
    if let Some(machine_id) = supplied {
        return Ok(machine_id);
    }
    if access == StoreAccess::ReadWrite {
        crate::sessions::machine::initialize().await.map_err(|_| {
            SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: "machine identity initialization failed".to_owned(),
            })
        })?;
    }
    Ok(crate::sessions::machine::current()
        .unwrap_or_default()
        .to_owned())
}

pub(super) async fn open_schema(
    store: &RemoteStore,
    access: StoreAccess,
) -> SessionResourceResult<(StoreId, StoreInitialization)> {
    let identity = match store.read_identity().await? {
        StoreIdentityRead::Present(snapshot) if snapshot.matches_build() => {
            StoreIdentityOutcome::Existing(snapshot.store_id)
        }
        StoreIdentityRead::Present(_) => {
            return Err(super::unsupported_behavior(
                "unrecognized PostgreSQL store schema",
            ));
        }
        StoreIdentityRead::Malformed => {
            return Err(super::super::session_codec::corrupt(
                "PostgreSQL store metadata is not interpretable",
            ));
        }
        StoreIdentityRead::Uninitialized if access == StoreAccess::ReadOnly => {
            return Err(super::unsupported_behavior(
                "read-only open of an uninitialized PostgreSQL store",
            ));
        }
        StoreIdentityRead::Uninitialized => store.initialize_store().await?,
    };
    if let StoreIdentityOutcome::Existing(id) = &identity {
        let shape = store
            .fetch_row(&StatementSpec::new(
                schema::VERIFY_SHAPE_SQL,
                vec![
                    Value::Text(super::super::session_schema::reserved_objects()),
                    Value::Text(id.as_str().to_owned()),
                ],
            ))
            .await?;
        if shape.as_deref().and_then(|row| sql::int_at(row, 0)) != Some(1) {
            return Err(super::unsupported_behavior(
                "PostgreSQL store schema differs from its initialized shape",
            ));
        }
    }
    Ok(open_verdict(identity))
}

pub(in crate::sessions::remote) fn open_verdict(
    outcome: StoreIdentityOutcome,
) -> (StoreId, StoreInitialization) {
    match outcome {
        StoreIdentityOutcome::Created(store_id) => {
            (store_id, StoreInitialization::CreatedByThisOpen)
        }
        StoreIdentityOutcome::Existing(store_id) => (store_id, StoreInitialization::Existing),
    }
}
