use std::sync::Arc;

use peri_agent::{
    agent::stages::{
        middleware_runner::run_before_agent, receive::run_receive, ReceiveInput, StageContext,
    },
    messages::{BaseMessage, ContentBlock, MessageContent},
    middleware::MiddlewareChain,
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
};

use super::test_support::ImageFixture;
use super::ImageMiddleware;

#[path = "../../at_mention/work_fixture.rs"]
mod work_fixture;

/// [回归测试] 后续 SDK 执行接收追加图片后触发 Micro，模型仍须收到图片载荷。
/// 历史缺口：图片准备仅挂在一次性的 before_agent，第二批输入只留下 @image 文本。
#[tokio::test]
async fn test_image_later_input_reaches_model_after_micro_compact() {
    use peri_agent::{
        agent::{
            compact_v2::CompactConfig,
            react::{ReactLLM, Reasoning, StreamingContext},
            stages::{run_react_loop, LoopResult},
            token::ContextBudget,
        },
        messages::ToolCallRequest,
        session::MessageQueue,
        tools::BaseTool,
    };
    use std::sync::Mutex;
    struct CapturingLlm {
        requests: Arc<Mutex<Vec<Vec<BaseMessage>>>>,
        pending_messages: Mutex<Vec<BaseMessage>>,
        queue: MessageQueue,
        next_input: BaseMessage,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CapturingLlm {
        fn prepare_reasoning(
            &self,
            messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
        ) -> peri_agent::error::AgentResult<peri_model::PreparedModelCall> {
            *self.pending_messages.lock().unwrap() = messages.to_vec();
            peri_agent::agent::model_bridge::AgentModelBridge::new(Arc::new(
                peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
                    "http://127.0.0.1:1".parse().unwrap(),
                    "fixture-unused-key",
                    "fixture-model",
                )),
            ))
            .prepare_reasoning(messages, tools)
        }

        async fn generate_prepared_reasoning(
            &self,
            _prepared: peri_model::PreparedModelCall,
            streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let messages = std::mem::take(&mut *self.pending_messages.lock().unwrap());
            self.generate_reasoning(&messages, &[], streaming).await
        }

        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(messages.to_vec());
            if requests.len() == 1 {
                self.queue.push(QueuedMessage::prompt(
                    MessageSource::UserInput,
                    self.next_input.clone(),
                ));
            }
            let mut reasoning = Reasoning::with_answer("", "收到");
            reasoning.usage = Some(peri_model::TokenUsage::new(160_000, 10));
            Ok(reasoning)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let image_path = dir.path().join("input.png");
    image::RgbImage::new(1, 1).save(&image_path).unwrap();
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        for i in 0..5 {
            transcript.append(BaseMessage::human(format!("旧任务 {i}")));
            transcript.append(BaseMessage::ai_with_tool_calls(
                "",
                vec![ToolCallRequest::new(
                    format!("call-{i}"),
                    "Bash",
                    serde_json::json!({}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(
                format!("call-{i}"),
                "x".repeat(4_000),
            ));
        }
    }
    let first = BaseMessage::human(format!("@image {}\n先看图片", image_path.display()));
    let later = BaseMessage::human(format!(
        "@image {}\n按照图中的数据修改",
        image_path.display()
    ));
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        first.clone(),
    ));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fixture = ImageFixture::new(dir.path()).await;
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(fixture.middleware()));
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_middleware_chain(Arc::new(chain))
    .with_llm(Arc::new(CapturingLlm {
        requests: Arc::clone(&requests),
        pending_messages: Mutex::new(Vec::new()),
        queue: session.queue().clone(),
        next_input: later.clone(),
    }))
    .with_context_budget(ContextBudget::new(200_000))
    .with_compact_config(CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    })
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
    let result = run_react_loop(ctx.clone(), 3).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "循环应正常完成：{result:?}"
    );
    let first_admission = ctx.session.turn.work_admission().unwrap().clone();
    let mut next_context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_recipient_lifecycle(1)
    .with_execution_admission_port(ctx.execution_admission_port().unwrap())
    .with_middleware_chain(Arc::clone(&ctx.runtime.middleware_chain))
    .with_llm(Arc::clone(&ctx.runtime.llm))
    .build();
    next_context.compact = ctx.compact.clone();
    let result = run_react_loop(next_context.clone(), 3).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "后续执行应完成：{result:?}"
    );
    assert_ne!(
        next_context
            .session
            .turn
            .work_admission()
            .unwrap()
            .admission_id,
        first_admission.admission_id,
        "新输入必须获得独立 SDK admission"
    );
    let ctx = next_context;
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "追加输入驱动第二次模型请求");
        use base64::Engine;
        let expected_image = ContentBlock::image_base64(
            "image/png",
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(image_path).unwrap()),
        );
        for (request, input) in [
            (&requests[0], &first),
            (&requests[1], &first),
            (&requests[1], &later),
        ] {
            let message = request
                .iter()
                .find(|message| message.id() == input.id())
                .unwrap();
            assert!(
                message.content_blocks().contains(&expected_image),
                "每批输入都必须向模型传递图片字节"
            );
            assert!(!message.content().contains("@image"), "附件引用应完成转换");
        }
        let transcript = ctx.session.transcript.read();
        assert!(
            transcript
                .entries()
                .iter()
                .any(|entry| transcript.flags(entry.id()).truncated),
            "必须实际执行 Micro Compact"
        );
        assert!(
            !transcript.flags(later.id()).truncated,
            "用户图片不参与 Micro 投影"
        );
    }
    fixture.shutdown().await;
}

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
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
    let mut chain = MiddlewareChain::new();
    let fixture = ImageFixture::new(dir.path()).await;
    chain.add(Box::new(fixture.middleware()));
    ctx.runtime.middleware_chain = Arc::new(chain);

    run_before_agent(&ctx, &[original.id()]).await.unwrap();

    {
        let transcript = ctx.session.transcript.read();
        assert_eq!(transcript.len(), 1);
        let updated = transcript.get(original.id()).unwrap().message();
        assert_eq!(updated.id(), original.id());
        assert!(matches!(updated, BaseMessage::Human { .. }));
        assert!(updated.content().contains("inspect"));
        assert!(updated.content().contains("Image not found:"));
        assert!(!updated.content().contains("@image"));
    }
    fixture.shutdown().await;
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
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
    let mut chain = MiddlewareChain::new();
    let fixture = ImageFixture::new(dir.path()).await;
    chain.add(Box::new(fixture.middleware()));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let received = run_receive(ReceiveInput {
        context: ctx.clone(),
    })
    .await
    .unwrap();
    run_before_agent(&ctx, &received.input_message_ids)
        .await
        .unwrap();
    {
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
                base64::engine::general_purpose::STANDARD
                    .encode(std::fs::read(image_path).unwrap())
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
    fixture.shutdown().await;
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
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
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
