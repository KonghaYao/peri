mod background_task_registry;

#[path = "task_projection.rs"]
mod task_projection;
use task_projection as projection;
#[path = "registration.rs"]
mod registration;
#[path = "settlement.rs"]
mod settlement;

#[path = "cancel.rs"]
mod cancel;

#[cfg(test)]
#[path = "projection_retention_test.rs"]
mod projection_retention_tests;
pub use background_task_registry::BackgroundRegistryError;

pub use background_task_registry::BgCancelHandle;

pub use background_task_registry::BackgroundTask;

pub use background_task_registry::BackgroundTaskStatus;

pub use background_task_registry::BgTaskInfo;

pub use background_task_registry::BackgroundTaskRegistry;
