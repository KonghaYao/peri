//! Deployment-scoped resources for Rivet-constructed actor instances (S5).
//!
//! Rivet builds actor values itself through [`rivetkit::Actor::create`], so a
//! deployment cannot inject non-serializable resources through a constructor or
//! actor input. The process that serves the registry installs them once, before
//! the first actor starts; every actor instance of that process reads the same
//! [`ActorDeployment`].
//!
//! S5 ships [`StaticDeployment`] over a shared [`SwarmStore`] (a
//! [`crate::store::MemorySwarmStore`] in tests and local development). S6
//! replaces the store with the Loom-backed port from `bootstrap`; the trait and
//! the install point stay the same.

use std::sync::{Arc, OnceLock};

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

/// Deployment with one shared store and one service configuration.
pub struct StaticDeployment {
    store: Arc<dyn SwarmStore>,
    config: ActorServiceConfig,
}

impl StaticDeployment {
    #[must_use]
    pub fn new(store: Arc<dyn SwarmStore>, config: ActorServiceConfig) -> Arc<Self> {
        Arc::new(Self { store, config })
    }
}

impl std::fmt::Debug for StaticDeployment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticDeployment")
            .field("config", &self.config)
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
}
