//! Pool 的持久化缓存准入、版本 fencing、RPC 缓存包装与失效策略。

use super::service;
use super::{McpClientPool, McpConnectionKey};
#[path = "cache_connection.rs"]
mod connection;
pub(crate) use connection::ConnectionResourceCache;
use connection::{CacheConnection, ConnectionCacheTicket};
#[cfg(test)]
#[path = "cache_fencing_test.rs"]
mod fencing_tests;
#[cfg(test)]
#[path = "cache_startup_test.rs"]
mod startup_tests;
use crate::mcp::config::{McpCachePolicy, McpServerConfig};
use rmcp::model::CacheScope;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResult;
use rmcp::model::Resource;
use rmcp::model::Tool;
use rmcp::service::Peer;
use rmcp::service::RoleClient;

#[path = "cache/cache_policy.rs"]
mod cache_policy;
pub(crate) use cache_policy::cache_scope_allows_persistence;
use cache_policy::AcpCacheScope;
