//! JavaScript port for the existing ACP Host. Protocol methods remain in peri-acp.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::RwLock;
use peri_acp::{
    host::{
        assemble::{assemble_wasm_server_config, WasmHostAssemblyInput},
        execution_admission::ReverseExecutionAdmission,
        spawn_acp_server, AcpHostHandle,
    },
    provider::{ConfigSource, LlmProvider},
    transport::{
        mpsc::mpsc_transport_pair, wire_bridge::WireBridge, AcpRequestBridge, AcpTransport,
    },
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use tokio::sync::Mutex;
use wasm_bindgen::prelude::*;

/// An ACP server with a raw JSON-RPC port for the embedding JavaScript host.
#[wasm_bindgen]
pub struct PeriWasmAcp {
    bridge: WireBridge,
    host: Mutex<AcpHostHandle>,
}

#[wasm_bindgen]
impl PeriWasmAcp {
    /// Start the same ACP Host used by native clients, with Emscripten capabilities.
    #[wasm_bindgen(js_name = start, experimental_tokio)]
    pub async fn start(config_json: String) -> Result<PeriWasmAcp, JsValue> {
        let input: serde_json::Value = serde_json::from_str(&config_json).map_err(js_error)?;
        let cwd = required_string(&input, "cwd")?;
        if !Path::new(&cwd).is_absolute() {
            return Err(JsValue::from_str("cwd must be an absolute path"));
        }
        // The JS host's workspace path is an identity in the virtual WASM filesystem.
        // Creating its empty directory lets ACP canonicalize it and reuse the injected
        // settings for session/new. Workspace file access still belongs to remote MCP.
        std::fs::create_dir_all(&cwd).map_err(js_error)?;
        let cwd = std::fs::canonicalize(&cwd)
            .map_err(js_error)?
            .to_string_lossy()
            .into_owned();
        let machine_id = required_string(&input, "machineId")?;
        let storage = input
            .get("storage")
            .ok_or_else(|| JsValue::from_str("storage is required"))?;
        let locator = required_string(storage, "url")?;
        let token = required_string(storage, "authToken")?;
        let settings = input
            .get("settings")
            .filter(|value| value.is_object())
            .ok_or_else(|| JsValue::from_str("settings must be an object"))?
            .to_string();

        // The trusted launcher supplies the complete settings document, as it does for
        // native ACP stdio. The absolute global path is only a configuration scope
        // identity here; it is never read because the document is injected.
        // Provider resolution and subsequent session updates share this source.
        let source = Arc::new(
            ConfigSource::load_injected_at(
                Path::new(&cwd),
                PathBuf::from("/.peri/settings.json"),
                settings,
            )
            .map_err(js_error)?,
        );
        let provider = LlmProvider::from_source(&source)
            .ok_or_else(|| JsValue::from_str("No LLM provider configured in settings"))?;
        let config = source.loaded_merged();

        let resources = peri_resources::Resources::open_turso_writable(
            &locator,
            token,
            cwd.clone().into(),
            &machine_id,
        )
        .await
        .map_err(js_error)?;
        let (session_resources, session_store_shutdown) = resources.into_parts();
        let mut host_config = assemble_wasm_server_config(WasmHostAssemblyInput {
            provider,
            peri_config: Arc::new(RwLock::new(config)),
            config_source: source,
            permission_mode: SharedPermissionMode::new(PermissionMode::Default),
            session_resources,
            session_store_shutdown: Some(Box::new(session_store_shutdown)),
            cwd,
        })
        .await;

        let (client, server) = mpsc_transport_pair();
        let bridge = WireBridge::new(client);
        let transport: Arc<dyn AcpTransport> = Arc::new(server);
        host_config.execution_admission_port = Some(Arc::new(ReverseExecutionAdmission::new(
            Arc::new(AcpRequestBridge(transport.clone())),
        )));
        let host = spawn_acp_server(transport, host_config);
        Ok(Self {
            bridge,
            host: Mutex::new(host),
        })
    }

    /// Submit one JSON-RPC frame to peri-acp without interpreting its method.
    #[wasm_bindgen(experimental_tokio)]
    pub async fn send(&self, frame: String) -> Result<(), JsValue> {
        let value = serde_json::from_str(&frame).map_err(js_error)?;
        self.bridge.send(value).await.map_err(js_error)
    }

    /// Receive one JSON-RPC frame emitted by the ACP Host.
    #[wasm_bindgen(experimental_tokio)]
    pub async fn recv(&self) -> Result<JsValue, JsValue> {
        match self.bridge.recv().await {
            Some(frame) => serde_json::to_string(&frame)
                .map(|frame| JsValue::from_str(&frame))
                .map_err(js_error),
            None => Ok(JsValue::NULL),
        }
    }

    /// Close client admission, then wait for the Host to drain and close Turso.
    #[wasm_bindgen(experimental_tokio)]
    pub async fn close(&self) -> Result<(), JsValue> {
        self.bridge.close().await;
        let report = self.host.lock().await.shutdown().await;
        if report.is_complete() {
            Ok(())
        } else {
            Err(JsValue::from_str(&format!("ACP Host shutdown: {report:?}")))
        }
    }
}

fn required_string(object: &serde_json::Value, field: &str) -> Result<String, JsValue> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| JsValue::from_str(&format!("{field} must be a nonempty string")))
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
