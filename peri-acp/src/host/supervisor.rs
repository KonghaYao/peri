//! Query the SDK's private per-process generation registry.
//! The socket and bearer come only from the trusted stdio launch environment.

#[cfg(unix)]
pub(super) async fn previous_generation_stopped(
    session_id: &str,
    generation_id: &str,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let socket = std::env::var_os("PERI_SUPERVISOR_SOCKET")
        .ok_or_else(|| "SDK process supervisor unavailable".to_owned())?;
    let token = std::env::var("PERI_SUPERVISOR_TOKEN")
        .map_err(|_| "SDK process supervisor credential unavailable".to_owned())?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SDK process supervisor credential invalid".into());
    }
    let query = async {
        let mut stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|error| format!("SDK process supervisor disconnected: {error}"))?;
        let request =
            serde_json::json!({"token":token,"sessionId":session_id,"generationId":generation_id});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|error| format!("SDK supervisor query failed: {error}"))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|error| format!("SDK supervisor reply failed: {error}"))?;
        if line.len() > 4096 {
            return Err("SDK supervisor reply too large".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(&line).map_err(|_| "SDK supervisor reply invalid".to_owned())?;
        if value.get("proven").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err("former Agent process tree has not been proven stopped".into())
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(25), query)
        .await
        .map_err(|_| "SDK supervisor proof timed out".to_owned())?
}

#[cfg(not(unix))]
pub(super) async fn previous_generation_stopped(
    _session_id: &str,
    _generation_id: &str,
) -> Result<(), String> {
    Err("SDK process supervisor unavailable on this platform".into())
}
