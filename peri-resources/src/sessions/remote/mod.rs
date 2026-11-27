//! PostgreSQL session storage. The same session, execution and OAuth ports back
//! native and hosted deployments. Writes commit atomically with their operation
//! receipt; reconnecting checks store identity before admitting new operations.

#![cfg_attr(not(test), allow(dead_code))]

#[cfg(test)]
#[path = "full_remote_test.rs"]
mod full_remote_tests;
#[cfg(test)]
mod postgres_fixture;

mod composition;
mod connection;
mod credentials;
mod remote_endpoint;
use remote_endpoint as endpoint;
mod remote_workspace_environment;
use remote_workspace_environment as environment;
mod remote_execution;
use remote_execution as execution;
mod remote_failure_class;
use remote_failure_class as failure;
mod generation;
mod ledger;
mod remote_store;
use remote_store as mutation;
mod remote_oauth_credentials;
use remote_oauth_credentials as oauth_credentials;
mod schema;

#[cfg(test)]
#[path = "remote_retention_test.rs"]
mod remote_retention_tests;

mod session_archive;
mod session_catalog;
mod session_codec;
mod session_data;
mod session_evidence;
mod session_history;
mod session_lifecycle;
mod session_mq;
mod session_read;
mod session_schema;
mod session_settlement;
mod session_sql;
mod session_write;
mod statement_spec;
use statement_spec as sql;

pub(crate) use composition::open_remote;
pub(crate) use composition::open_remote_in_environment;
pub(crate) use credentials::SessionStoreCredential;
pub(crate) use credentials::{CredentialError, CredentialSource};
pub(crate) use endpoint::{EndpointError, RemoteEndpoint, RemoteEngine};
pub use environment::RemoteWorkspaceEnvironment;
#[cfg(test)]
pub(crate) use failure::RemoteFailureClass;

#[cfg(test)]
#[path = "remote_test.rs"]
mod tests;

#[cfg(test)]
#[path = "ledger_test.rs"]
mod ledger_tests;

#[cfg(test)]
#[path = "mutation_test.rs"]
mod mutation_tests;

#[cfg(test)]
#[path = "schema_test.rs"]
mod schema_tests;

#[cfg(test)]
#[path = "initialization_test.rs"]
mod initialization_tests;

#[cfg(test)]
#[path = "session_shape_test.rs"]
mod session_shape_tests;

#[cfg(test)]
#[path = "session_history_page_test.rs"]
mod session_history_page_tests;

// History V2 的完整回放与 frozen 读取夹具；与 V1 分页测试并存。
#[cfg(test)]
#[path = "session_history_replay_test.rs"]
mod session_history_replay_tests;

#[cfg(test)]
#[path = "session_mq_test.rs"]
mod session_mq_tests;

#[cfg(test)]
#[path = "session_work_state_test.rs"]
mod session_work_state_tests;

#[cfg(test)]
#[path = "session_rewind_candidates_test.rs"]
mod session_rewind_candidates_tests;

#[cfg(test)]
#[path = "session_history_rewind_test.rs"]
mod session_history_rewind_tests;

#[cfg(test)]
#[path = "session_child_guard_test.rs"]
mod session_child_guard_tests;

#[cfg(test)]
#[path = "session_tree_race_test.rs"]
mod session_tree_race_tests;

#[cfg(test)]
#[path = "session_close_test.rs"]
mod session_close_tests;

#[cfg(test)]
#[path = "recovery_fixture_test.rs"]
mod recovery_fixture_tests;

#[cfg(test)]
#[path = "connection_recovery_test.rs"]
mod connection_recovery_tests;

#[cfg(test)]
#[path = "connection_close_test.rs"]
mod connection_close_tests;

#[cfg(test)]
#[path = "cloud_test.rs"]
mod cloud_tests;

#[cfg(test)]
#[path = "cloud_mutation_test.rs"]
mod cloud_mutation_tests;

#[cfg(test)]
#[path = "cloud_session_test.rs"]
mod cloud_session_tests;

#[cfg(test)]
#[path = "cloud_history_test.rs"]
mod cloud_history_tests;

#[cfg(test)]
#[path = "cloud_lifecycle_test.rs"]
mod cloud_lifecycle_tests;

#[cfg(test)]
#[path = "cloud_identity_test.rs"]
mod cloud_identity_tests;

#[cfg(test)]
#[path = "cloud_recovery_test.rs"]
mod cloud_recovery_tests;

#[cfg(test)]
#[path = "cloud_limit_test.rs"]
mod cloud_limit_tests;

#[cfg(test)]
#[path = "cloud_deployment_test.rs"]
mod cloud_deployment_tests;

#[cfg(test)]
#[path = "cloud_deployment_child_test.rs"]
mod cloud_deployment_child_tests;
