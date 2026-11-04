//! Test-only History V2 fixtures for session/client tests.
//!
//! The TUI no longer has a non-V2 `session/load` path, so ordinary session tests
//! must negotiate V2 and answer with a valid `initial` payload.

use peri_acp_types::history_v2::{HISTORY_V2_VERSION, HistoryLimits};
use serde_json::{Value, json};

use super::AcpTuiClient;

/// Install a V2-negotiated connection for tests (no process-global atoms).
///
/// Load tests must not accidentally queue into a scheduler installed by another
/// test: clear the global outbound slot so `submit_load` uses the direct path.
pub(crate) fn install_history_v2(client: &AcpTuiClient) {
    *crate::kit::atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    client.install_history_v2_for_test(true, HistoryLimits::default());
}

/// Minimal valid `_peri/history` initial response for `session_id`.
pub(crate) fn history_v2_initial(session_id: &str) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "test-runtime",
                "deliveryId": "test-delivery",
                "liveBoundary": {"streamId": "test-stream", "afterSeq": "0"},
                "page": {
                    "version": HISTORY_V2_VERSION,
                    "kind": "window",
                    "sessionId": session_id,
                    "windowId": "test-window",
                    "snapshotToken": "test-snapshot",
                    "historyRevision": "0",
                    "committedTail": null,
                    "projectionVersion": 1,
                    "readTicket": "test-ticket",
                    "beforeCursor": null,
                    "afterCursor": null,
                    "items": [],
                    "coverage": {"kind": "emptySnapshot"},
                    "progress": {
                        "reason": "complete",
                        "scannedEntries": 0,
                        "consumedEntries": 0,
                    },
                },
            }
        }
    })
}

/// Merge the V2 initial payload into an existing `session/load` response while
/// preserving extra `_meta` fields the test asserts on. The echoed `requestKey`
/// is copied from the actual request: the client rejects a mismatched echo.
pub(crate) fn with_history_v2_initial(
    mut response: Value,
    session_id: &str,
    request: &Value,
) -> Value {
    let sent_key = request["_meta"]["peri.history"]["requestKey"].clone();
    let mut initial = history_v2_initial(session_id);
    if !sent_key.is_null() {
        initial["_meta"]["peri.history"]["requestKey"] = sent_key.clone();
    }
    let Some(initial_meta) = initial.get("_meta").cloned() else {
        return response;
    };
    let Some(object) = response.as_object_mut() else {
        return initial;
    };
    let meta = object
        .entry("_meta")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let (Some(meta), Some(initial_meta)) = (meta.as_object_mut(), initial_meta.as_object()) {
        for (key, value) in initial_meta {
            meta.insert(key.clone(), value.clone());
        }
    }
    response
}
