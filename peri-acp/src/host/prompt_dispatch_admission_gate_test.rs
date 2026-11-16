use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct Gate {
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
}

static GATES: LazyLock<Mutex<HashMap<String, Arc<Gate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn install(session_id: &str) -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    GATES
        .lock()
        .unwrap()
        .insert(session_id.into(), gate.clone());
    gate
}

pub(in crate::host) async fn wait(session_id: &str) {
    let gate = GATES.lock().unwrap().remove(session_id);
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.release.notified().await;
    }
}
