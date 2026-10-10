use super::*;

fn request(path: &str, max_size: usize) -> ReadImageRequest {
    ReadImageRequest {
        path: path.to_owned(),
        max_size,
    }
}

#[test]
fn image_reads_magic_not_extension_and_resolves_workspace_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.data");
    let image = image::RgbImage::new(1, 1);
    image
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    let result = load_image(dir.path().to_str().unwrap(), request("photo.data", 1024)).unwrap();
    assert_eq!(result.media_type, "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(result.data)
            .unwrap(),
        std::fs::read(path).unwrap()
    );
}

#[test]
fn image_missing_file_is_a_safe_typed_failure() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        load_image(
            dir.path().to_str().unwrap(),
            request("private/missing.png", 1024)
        ),
        Err(ImageReadError::NotFound)
    ));
}

#[test]
fn image_directory_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        load_image(dir.path().to_str().unwrap(), request(".", 1024)),
        Err(ImageReadError::NotFile)
    ));
}

#[test]
fn image_size_limit_never_returns_truncated_image_data() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("large.png"), [0; 128]).unwrap();
    assert!(matches!(
        load_image(dir.path().to_str().unwrap(), request("large.png", 127)),
        Err(ImageReadError::TooLarge)
    ));
}

#[test]
fn image_caller_cannot_raise_provider_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::File::create(dir.path().join("large.png"))
        .unwrap()
        .set_len(MAX_IMAGE_BYTES as u64 + 1)
        .unwrap();
    assert!(matches!(
        load_image(
            dir.path().to_str().unwrap(),
            request("large.png", usize::MAX)
        ),
        Err(ImageReadError::TooLarge)
    ));
}

#[test]
fn image_text_disguised_as_png_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fake.png"), "not an image").unwrap();
    assert!(matches!(
        load_image(dir.path().to_str().unwrap(), request("fake.png", 1024)),
        Err(ImageReadError::NotImage)
    ));
}

#[test]
fn image_supported_mime_types_preserve_existing_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    for (data, mime) in [
        (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
        (b"\xff\xd8\xff".as_slice(), "image/jpeg"),
        (b"GIF89a".as_slice(), "image/gif"),
        (b"RIFF\0\0\0\0WEBP".as_slice(), "image/webp"),
    ] {
        std::fs::write(dir.path().join("image"), data).unwrap();
        let response = load_image(dir.path().to_str().unwrap(), request("image", 1024)).unwrap();
        assert_eq!(response.media_type, mime);
    }
    std::fs::write(dir.path().join("image"), b"BM").unwrap();
    assert!(matches!(
        load_image(dir.path().to_str().unwrap(), request("image", 1024)),
        Err(ImageReadError::NotImage)
    ));
}

#[tokio::test]
async fn image_custom_request_uses_wire_without_adding_model_tools() {
    use rmcp::{
        model::{ClientRequest, ServerResult},
        ClientLifecycleMode,
    };
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    image::RgbImage::new(1, 1).save(&path).unwrap();
    let server = crate::WorkspaceMcpServer::new(dir.path().to_string_lossy(), None);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_task = tokio::spawn(async move {
        rmcp::serve_server(server, tokio::io::split(server_io))
            .await
            .unwrap()
            .waiting()
            .await
    });
    let mut client = tokio::time::timeout(
        Duration::from_secs(2),
        rmcp::serve_client_with_lifecycle(
            (),
            tokio::io::split(client_io),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                legacy_version: None,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let peer = client.peer();
    let tools = peer.list_all_tools().await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        [
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "folder_operations",
            "Bash"
        ]
    );
    let result = peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            READ_IMAGE_METHOD,
            Some(serde_json::json!({ "path": "image.png", "max_size": 1024 })),
        )))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom response required");
    };
    let image = result.result_as::<ReadImageResponse>().unwrap().unwrap();
    assert_eq!(image.media_type, "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(image.data)
            .unwrap(),
        std::fs::read(path).unwrap()
    );
    let error = peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            READ_IMAGE_METHOD,
            Some(serde_json::json!({ "path": "image.png", "max_size": -1 })),
        )))
        .await
        .unwrap_err();
    assert!(
        matches!(error, rmcp::ServiceError::McpError(error) if error.code == rmcp::model::ErrorCode::INVALID_PARAMS)
    );
    let _ = client.close_with_timeout(Duration::from_millis(500)).await;
    if tokio::time::timeout(Duration::from_millis(500), &mut server_task)
        .await
        .is_err()
    {
        server_task.abort();
        let _ = server_task.await;
    }
}
