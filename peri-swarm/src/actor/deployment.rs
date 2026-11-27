//! Deployment-scoped resources for Rivet-constructed actor instances (S5).
//!
//! Rivet builds actor values itself through [`rivetkit::Actor::create`], so a
//! deployment cannot inject non-serializable resources through a constructor or
//! actor input. The process that serves the registry installs them once, before
//! the first actor starts; every actor instance of that process reads the same
//! [`ActorDeployment`].
//!
//! S5 ships [`StaticDeployment`] over a shared [`SwarmStore`] (a
//! [`crate::store::MemorySwarmStore`] in tests and local development). S6c1a
//! installs it with the independent Swarm state store
//! ([`crate::store::SwarmStateStore`]) and an [`ActorServiceConfig`] that
//! carries the remote Peri session-store deployment; the trait and the install
//! point stay the same.

use std::sync::{Arc, OnceLock};

use peri_acp_types::session_resources::SessionResources;

use super::service::ActorServiceConfig;
use crate::store::SwarmStore;

/// Resources every actor instance of this process shares.
///
/// The store must be the same instance the gateway adapter reads: the gateway
/// maps `logicalId` to `rootSessionId` and checks existence through it, while
/// the actor owns every write.
pub trait ActorDeployment: Send + Sync {
    fn store(&self) -> Arc<dyn SwarmStore>;
    fn service_config(&self) -> ActorServiceConfig;

    /// Deployment-owned session-store business handle.
    ///
    /// Every ACP child clones this handle instead of opening its own facade and
    /// connection pool. The deployment keeps the only close right; children
    /// never shut the shared store down. `None` keeps the per-child open path
    /// (local SQLite, tests, standalone actors).
    fn session_resources(&self) -> Option<Arc<dyn SessionResources>> {
        None
    }
}

static DEPLOYMENT: OnceLock<Arc<dyn ActorDeployment>> = OnceLock::new();

/// Install the process deployment, or return the one already installed.
///
/// Returns the deployment the process will use so a caller that lost an install
/// race observes the winner instead of silently using its own store.
pub fn install_deployment(deployment: Arc<dyn ActorDeployment>) -> Arc<dyn ActorDeployment> {
    Arc::clone(DEPLOYMENT.get_or_init(move || deployment))
}

/// The installed deployment, if the process is serving actors.
#[must_use]
pub fn current_deployment() -> Option<Arc<dyn ActorDeployment>> {
    DEPLOYMENT.get().cloned()
}

/// Deployment with one shared store, one service configuration and an optional
/// deployment-owned session-store handle.
pub struct StaticDeployment {
    store: Arc<dyn SwarmStore>,
    config: ActorServiceConfig,
    session_resources: Option<Arc<dyn SessionResources>>,
}

impl StaticDeployment {
    #[must_use]
    pub fn new(store: Arc<dyn SwarmStore>, config: ActorServiceConfig) -> Arc<Self> {
        Self::new_with_session_resources(store, config, None)
    }

    /// Install with the deployment's session-store business handle so every
    /// ACP child shares one facade and pool.
    #[must_use]
    pub fn new_with_session_resources(
        store: Arc<dyn SwarmStore>,
        config: ActorServiceConfig,
        session_resources: Option<Arc<dyn SessionResources>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            config,
            session_resources,
        })
    }
}

impl std::fmt::Debug for StaticDeployment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticDeployment")
            .field("config", &self.config)
            .field(
                "shared_session_resources",
                &self.session_resources.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ActorDeployment for StaticDeployment {
    fn store(&self) -> Arc<dyn SwarmStore> {
        Arc::clone(&self.store)
    }

    fn service_config(&self) -> ActorServiceConfig {
        self.config.clone()
    }

    fn session_resources(&self) -> Option<Arc<dyn SessionResources>> {
        self.session_resources.as_ref().map(Arc::clone)
    }
}
