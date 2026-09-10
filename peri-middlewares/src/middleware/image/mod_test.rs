use std::sync::Arc;

use peri_agent::{
    agent::stages::{middleware_runner::run_before_agent, StageContext},
    messages::{BaseMessage, MessageContent},
    middleware::MiddlewareChain,
    session::{FrozenContext, Session},
};

use super::ImageMiddleware;

#[tokio::test]
async fn image_replacement_reaches_transcript_with_the_original_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let missing_image = dir.path().join("missing.png");
    let cwd: Arc<str> = Arc::from(dir.path().to_str().unwrap());
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let original = BaseMessage::human(MessageContent::text(format!(
        "inspect @image {}",
        missing_image.display()
    )));
    session.transcript().write().append(original.clone());
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ImageMiddleware::new()));
    ctx.runtime.middleware_chain = Arc::new(chain);

    run_before_agent(&ctx).await.unwrap();

    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let updated = transcript.get(original.id()).unwrap().message();
    assert_eq!(updated.id(), original.id());
    assert!(matches!(updated, BaseMessage::Human { .. }));
    assert!(updated.content().contains("inspect"));
    assert!(updated.content().contains("Image not found:"));
    assert!(!updated.content().contains("@image"));
}
