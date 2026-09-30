use std::{sync::Arc, time::Duration};

use base64::Engine as _;
use peri_mcp_workspace::image::{
    ImageReadError, ReadImageRequest, ReadImageResponse, MAX_IMAGE_BYTES, READ_IMAGE_METHOD,
};
use rmcp::{
    model::{CancelledNotificationParam, ClientRequest, CustomRequest, RequestId, ServerResult},
    service::{Peer, PeerRequestOptions, RoleClient},
};

use crate::mcp::{config::ConfigSource, ClientStatus, McpClientPool};

const READ_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) struct ImageBytes {
    pub data: Vec<u8>,
    pub media_type: String,
}

struct PendingImageRead {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for PendingImageRead {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        peer.notify_cancelled(CancelledNotificationParam::new(
                            Some(id),
                            Some("image read cancelled".into()),
                        )),
                    )
                    .await;
                });
            }
        }
    }
}

pub(super) async fn read_image(
    pool: Option<&McpClientPool>,
    session_id: Option<&str>,
    path: &str,
    max_size: usize,
) -> Result<ImageBytes, String> {
    let unavailable = || "Workspace image capability unavailable".to_owned();
    let pool = pool.ok_or_else(unavailable)?;
    let session_id = session_id.ok_or_else(unavailable)?;
    if workspace_closed(pool) {
        return Err(unavailable());
    }
    let client = pool
        .get_client_visible_to("workspace", Some(session_id))
        .ok_or_else(unavailable)?;
    if !matches!(client.status, ClientStatus::Connected)
        || !matches!(client.source.as_ref(), Some(ConfigSource::Builtin { instance }) if instance == "workspace")
    {
        return Err(unavailable());
    }
    let peer = client.peer.as_ref().ok_or_else(unavailable)?;
    let max_size = max_size.min(MAX_IMAGE_BYTES);
    let params = serde_json::to_value(ReadImageRequest {
        path: path.to_owned(),
        max_size,
    })
    .map_err(|_| "Invalid image request".to_owned())?;
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    let request = ClientRequest::CustomRequest(CustomRequest::new(READ_IMAGE_METHOD, Some(params)));
    let handle = tokio::time::timeout_at(
        deadline,
        peer.send_request_with_option(request, PeerRequestOptions::no_options()),
    )
    .await
    .map_err(|_| "Workspace image read timed out".to_owned())?
    .map_err(|_| "Workspace image read failed".to_owned())?;
    let mut pending = PendingImageRead {
        peer: peer.clone(),
        id: Some(handle.id.clone()),
    };
    let response = tokio::time::timeout_at(deadline, handle.await_response())
        .await
        .map_err(|_| "Workspace image read timed out".to_owned())?
        .map_err(|_| "Workspace image read failed".to_owned())?;
    pending.id.take();
    if workspace_closed(pool)
        || !pool
            .get_client_visible_to("workspace", Some(session_id))
            .is_some_and(|current| Arc::ptr_eq(&current, &client))
    {
        return Err(unavailable());
    }
    let ServerResult::CustomResult(result) = response else {
        return Err("Invalid workspace image response".to_owned());
    };
    let image = serde_json::from_value::<ReadImageResponse>(result.0)
        .map_err(|_| "Invalid workspace image response".to_owned())?
        .map_err(|error| {
            match error {
                ImageReadError::NotFound => "Image not found: attachment",
                ImageReadError::NotFile => "Not a file: image attachment",
                ImageReadError::CannotRead => "Cannot read image file",
                ImageReadError::TooLarge => "Image too large: exceeds attachment size limit",
                ImageReadError::NotImage => "Not an image: attachment",
            }
            .to_owned()
        })?;
    if !matches!(
        image.media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) || image.data.len() > max_size.div_ceil(3) * 4
    {
        return Err("Invalid workspace image response".to_owned());
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(image.data)
        .map_err(|_| "Invalid workspace image response".to_owned())?;
    if data.len() > max_size {
        return Err("Image too large: exceeds attachment size limit".to_owned());
    }
    Ok(ImageBytes {
        data,
        media_type: image.media_type,
    })
}

fn workspace_closed(pool: &McpClientPool) -> bool {
    pool.builtin_instance_context()
        .is_some_and(|context| crate::mcp::builtin::is_closed("workspace", &context.closed))
}

#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
