//! Ignored live smoke against a real gateway.
//!
//! Run explicitly (never in CI):
//!
//! ```text
//! PERI_SWARM_DASHBOARD_IT_BASE_URL=http://127.0.0.1:8080 \
//! PERI_SWARM_DASHBOARD_IT_API_KEY=... \
//! cargo test -p peri-swarm-dashboard --test live_gateway -- --ignored
//! ```
//!
//! Missing variables skip cleanly. The key is only passed to the client; it is
//! never printed, including in assertion or error messages.

use peri_swarm_dashboard::api::{HttpSwarmApi, SwarmApi};

#[tokio::test]
#[ignore = "requires a live gateway and PERI_SWARM_DASHBOARD_IT_* credentials"]
async fn health_then_list_sessions() {
    let Ok(base_url) = std::env::var("PERI_SWARM_DASHBOARD_IT_BASE_URL") else {
        eprintln!("PERI_SWARM_DASHBOARD_IT_BASE_URL is not set; skipping");
        return;
    };
    let Ok(api_key) = std::env::var("PERI_SWARM_DASHBOARD_IT_API_KEY") else {
        eprintln!("PERI_SWARM_DASHBOARD_IT_API_KEY is not set; skipping");
        return;
    };

    let api = HttpSwarmApi::new(&base_url, &api_key).expect("build live client");
    let health = api.health().await.expect("GET /healthz");
    assert_eq!(health.status, "ok");

    let page = api.list_sessions(1).await.expect("GET sessions");
    assert!(page.data.len() <= 1, "limit=1 returned too many sessions");
    assert_eq!(
        api.contract_version().as_deref(),
        Some("1"),
        "gateway contract header"
    );
}
