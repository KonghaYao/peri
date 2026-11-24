use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::support::fake_engine::{Engine, Event};
use crate::support::gateway::Gateway;

#[tokio::test]
async fn status_guides_install_serve_explicit_connect_and_recovery_without_owning_the_engine(
) -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let suggested = format!("http://{address}");
    let serve = format!("kitesurf serve --port {}", address.port());
    let connect = format!("browser_connect(url: \"{suggested}\")");
    let mut gateway = Gateway::start(&suggested, &[]).await?;
    let id = gateway.init().await?;
    let missing = gateway.success(&id, "browser_status", json!({})).await?;
    assert_eq!(missing["kitesurf"]["installed"], false);
    assert_eq!(missing["kitesurf"]["version"], Value::Null);
    assert_eq!(
        missing["kitesurf"]["installInstruction"],
        "Ask the user before running installCommand."
    );
    assert_eq!(
        missing["nextSteps"],
        json!([missing["kitesurf"]["installCommand"], serve, connect])
    );
    assert_eq!(
        missing["recommendedEndpoint"],
        json!({"url": suggested, "listening": false})
    );
    assert_eq!(missing["monitor"], "waiting_for_connect");
    assert_eq!(missing["gatewaySock"], Value::Null);
    assert_eq!(missing["monitorIndex"], format!("{}/", gateway.base));
    assert_eq!(missing["platform"]["os"], std::env::consts::OS);
    assert_eq!(missing["platform"]["arch"], std::env::consts::ARCH);
    assert_eq!(missing["endpoint"], Value::Null);
    assert_eq!(missing["connected"], false);
    assert_eq!(missing["pages"], json!([]));

    let root = gateway.install_root();
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::write(root.join("bin/kitesurf"), b"fixture: not executable")?;
    std::fs::write(root.join("current-version.txt"), b"9.8.7-fixture\n")?;
    let installed = gateway.success(&id, "browser_status", json!({})).await?;
    assert_eq!(installed["kitesurf"]["installed"], true);
    assert_eq!(installed["kitesurf"]["version"], "9.8.7-fixture");
    assert_eq!(installed["kitesurf"]["onPath"], false);
    assert_eq!(installed["nextSteps"], json!([serve, connect]));
    gateway
        .error_code(
            &id,
            "browser_connect",
            json!({"url": &suggested}),
            "connect_failed",
        )
        .await?;
    gateway
        .error_code(
            &id,
            "browser_connect",
            json!({"url": "https://127.0.0.1:1"}),
            "invalid_argument",
        )
        .await?;
    let mut engine = Engine::start(listener).await?;
    let listening = gateway.success(&id, "browser_status", json!({})).await?;
    assert_eq!(listening["recommendedEndpoint"]["listening"], true);
    assert_eq!(listening["nextSteps"], json!([connect]));
    assert_eq!(listening["endpoint"], Value::Null);
    assert_eq!(listening["connected"], false);
    assert_eq!(listening["engine"], Value::Null);
    assert!(engine.events().iter().all(|event| *event == Event::Probe));

    let canonical = format!("ws://{address}/devtools/browser");
    let bound = gateway
        .success(
            &id,
            "browser_connect",
            json!({"url": format!("{suggested}/ignored?owner=ignored")}),
        )
        .await?;
    assert_eq!(bound["endpoint"], canonical);
    assert_eq!(
        gateway
            .success(&id, "browser_connect", json!({"url": canonical}))
            .await?["alreadyConnected"],
        true
    );
    let connected = gateway.success(&id, "browser_status", json!({})).await?;
    assert_eq!(connected["connected"], true);
    assert_eq!(connected["monitor"], "connected");
    assert_eq!(connected["engine"]["pageCount"], Value::Null);
    assert_eq!(
        connected["engine"]["version"]["Browser"],
        "Kitesurf/fixture"
    );
    assert_eq!(connected["pageCount"], 0);
    assert_eq!(connected["activePageId"], Value::Null);
    assert_eq!(connected["nextSteps"], json!([]));
    assert!(engine.events().iter().all(|event| *event == Event::Probe));
    engine.shutdown().await?;
    let dropped = gateway.success(&id, "browser_status", json!({})).await?;
    assert_eq!(dropped["connected"], false);
    assert_eq!(dropped["endpoint"], canonical);
    assert_eq!(dropped["nextSteps"], json!([serve]));
    assert_eq!(dropped["sessionId"], missing["sessionId"]);
    gateway.shutdown().await
}
