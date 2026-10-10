use super::*;
use crate::middleware::image::test_support::ImageFixture;

#[tokio::test]
async fn image_missing_workspace_never_reads_an_existing_host_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.png");
    image::RgbImage::new(1, 1).save(&path).unwrap();
    let error = read_image(None, Some("image-session"), path.to_str().unwrap(), 1024)
        .await
        .err()
        .unwrap();
    assert_eq!(error, "Workspace image capability unavailable");
}

#[tokio::test]
async fn image_workspace_cwd_is_the_only_relative_path_base() {
    let dir = tempfile::tempdir().unwrap();
    image::RgbImage::new(1, 1)
        .save(dir.path().join("input.png"))
        .unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    let result = read_image(
        Some(&fixture.pool),
        Some("image-session"),
        "input.png",
        1024,
    )
    .await
    .unwrap();
    assert_eq!(result.media_type, "image/png");
    assert_eq!(
        result.data,
        std::fs::read(dir.path().join("input.png")).unwrap()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_workspace_owned_by_another_session_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    fixture
        .pool
        .acp_owners
        .write()
        .insert("workspace".to_owned(), "other-session".to_owned());
    let result = read_image(
        Some(&fixture.pool),
        Some("image-session"),
        "input.png",
        1024,
    )
    .await;
    assert_eq!(
        result.err().unwrap(),
        "Workspace image capability unavailable"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_same_named_nonbuiltin_server_cannot_supply_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    let mut client = (*fixture.pool.get_client("workspace").unwrap()).clone();
    client.source = None;
    fixture
        .pool
        .clients
        .write()
        .insert("workspace".to_owned(), Arc::new(client));
    let result = read_image(
        Some(&fixture.pool),
        Some("image-session"),
        "input.png",
        1024,
    )
    .await;
    assert_eq!(
        result.err().unwrap(),
        "Workspace image capability unavailable"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_construct_time_workspace_disable_overrides_open_pool() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    let disabled = std::collections::HashSet::from(["WorkspaceMiddleware".to_owned()]);
    let middleware = crate::middleware::image::ImageMiddleware::new().with_mcp_pool(
        fixture.pool.clone(),
        "image-session".to_owned(),
        &disabled,
    );
    assert!(middleware.pool.is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_pool_workspace_disable_is_also_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    let mut context =
        crate::mcp::builtin::context::BuiltinInstanceContext::new(dir.path().to_string_lossy());
    context.closed.insert("workspace".to_owned());
    fixture
        .pool
        .set_builtin_instance_context(Arc::new(context))
        .unwrap();
    let result = read_image(
        Some(&fixture.pool),
        Some("image-session"),
        "input.png",
        1024,
    )
    .await;
    assert_eq!(
        result.err().unwrap(),
        "Workspace image capability unavailable"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_disconnected_workspace_does_not_fall_back_to_host() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ImageFixture::new(dir.path()).await;
    let mut client = (*fixture.pool.get_client("workspace").unwrap()).clone();
    client.status = ClientStatus::Disabled;
    fixture
        .pool
        .clients
        .write()
        .insert("workspace".to_owned(), Arc::new(client));
    let result = read_image(
        Some(&fixture.pool),
        Some("image-session"),
        "input.png",
        1024,
    )
    .await;
    assert_eq!(
        result.err().unwrap(),
        "Workspace image capability unavailable"
    );
    fixture.shutdown().await;
}
