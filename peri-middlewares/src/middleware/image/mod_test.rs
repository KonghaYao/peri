use std::sync::Arc;

use peri_agent::{
    agent::stages::{
        middleware_runner::run_before_agent, receive::run_receive, ReceiveInput, StageContext,
    },
    messages::{BaseMessage, ContentBlock, MessageContent},
    middleware::MiddlewareChain,
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
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

    run_before_agent(&ctx, &[original.id()]).await.unwrap();

    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let updated = transcript.get(original.id()).unwrap().message();
    assert_eq!(updated.id(), original.id());
    assert!(matches!(updated, BaseMessage::Human { .. }));
    assert!(updated.content().contains("inspect"));
    assert!(updated.content().contains("Image not found:"));
    assert!(!updated.content().contains("@image"));
}

/// [回归测试] 一次 Receive 接收多条用户输入时，附件不能只处理最后一条。
#[tokio::test]
async fn test_image_batch_prepares_first_input_and_never_reloads_history() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.png");
    let image_path = dir.path().join("new.png");
    for path in [&old_path, &image_path] {
        image::RgbImage::new(1, 1).save(path).unwrap();
    }
    let old = BaseMessage::human(format!("old @image {}", old_path.display()));
    let first = BaseMessage::human(MessageContent::blocks(vec![
        ContentBlock::text(format!(
            "inspect @image {} @image {}",
            image_path.display(),
            dir.path().join("missing.png").display()
        )),
        ContentBlock::image_base64("image/png", "already-attached"),
    ]));
    let last = BaseMessage::human("普通文本");
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    session.transcript().write().append(old.clone());
    session
        .transcript()
        .write()
        .append(BaseMessage::ai("之前的回复"));
    for input in [&first, &last] {
        session.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            input.clone(),
        ));
    }
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ImageMiddleware::new()));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let received = run_receive(ReceiveInput {
        context: ctx.clone(),
    })
    .await
    .unwrap();
    run_before_agent(&ctx, &received.input_message_ids)
        .await
        .unwrap();
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 4, "附件转换不增删或重排消息");
    let updated = transcript.get(first.id()).unwrap().message();
    assert_eq!(updated.id(), first.id(), "批次首条保留原消息身份");
    assert!(!updated.content().contains("@image"), "首条附件标记已处理");
    assert!(
        updated.content().contains("Image not found:"),
        "错误落在原输入"
    );
    let images = updated
        .content_blocks()
        .into_iter()
        .filter(|block| matches!(block, ContentBlock::Image { .. }))
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 2, "文件图片与原有粘贴附件均保留");
    assert_eq!(
        images[0],
        ContentBlock::image_base64("image/png", "already-attached"),
        "已有附件载荷不改变"
    );
    use base64::Engine;
    assert_eq!(
        images[1],
        ContentBlock::image_base64(
            "image/png",
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(image_path).unwrap())
        ),
        "本批首条图片必须读取真实文件"
    );
    assert_eq!(
        serde_json::to_value(transcript.get(last.id()).unwrap().message()).unwrap(),
        serde_json::to_value(&last).unwrap(),
        "末条普通内容与身份不改变"
    );
    assert_eq!(
        serde_json::to_value(transcript.get(old.id()).unwrap().message()).unwrap(),
        serde_json::to_value(&old).unwrap(),
        "旧历史的附件引用不能重读或改写"
    );
}

#[tokio::test]
async fn test_image_explicit_empty_batch_does_not_fall_back_to_history() {
    let dir = tempfile::tempdir().unwrap();
    let original = BaseMessage::human(format!(
        "history @image {}",
        dir.path().join("missing.png").display()
    ));
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    session.transcript().write().append(original.clone());
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ImageMiddleware::new()));
    ctx.runtime.middleware_chain = Arc::new(chain);
    run_before_agent(&ctx, &[]).await.unwrap();
    assert_eq!(
        serde_json::to_value(
            ctx.session
                .transcript
                .read()
                .get(original.id())
                .unwrap()
                .message()
        )
        .unwrap(),
        serde_json::to_value(&original).unwrap(),
        "明确没有新输入时不能回退到最后一条历史消息"
    );
}
