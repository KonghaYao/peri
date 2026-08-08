use langfuse_client::{IngestionEvent, LangfuseError};
use parking_lot::Mutex;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::drop_telemetry::LangfuseDropRegistry;
use super::session_like::LangfuseSessionLike;

pub struct FakeLangfuseSession {
    events: Mutex<Vec<IngestionEvent>>,
    drop_registry: LangfuseDropRegistry,
    session_id: String,
}

impl FakeLangfuseSession {
    pub fn new(session_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            drop_registry: LangfuseDropRegistry::default(),
            session_id: session_id.into(),
        })
    }

    pub fn events_snapshot(&self) -> Vec<IngestionEvent> {
        self.events.lock().clone()
    }

    pub fn event_count(&self) -> usize {
        self.events.lock().len()
    }
}

impl LangfuseSessionLike for FakeLangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.events.lock().push(event);
        Ok(())
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = Result<(), LangfuseError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }
}
