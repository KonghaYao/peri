//! Terminal dashboard for the Peri Swarm gateway.
//!
//! The dashboard is a pure HTTP client: it never imports the gateway crate and
//! treats the wire contract as the only source of truth.

pub mod api;
pub mod app;
pub mod config;
pub mod metrics;
pub mod observability;
pub mod poll;
pub mod text_field;
pub mod ui;
pub mod view_model;
pub mod worker;

#[cfg(test)]
pub(crate) mod stub_http;
#[cfg(test)]
pub(crate) mod test_support;
