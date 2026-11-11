use super::*;
use crate::{
    agent::model_bridge::AgentModelBridge,
    messages::{ContentBlock, ImageSource},
    session::{FrozenContext, Session},
    tools::{ToolContext, ToolOutput},
};
use peri_acp_types::tool_outcome::RawToolResult;
use peri_model::{Model, ModelRequest, OpenAiConfig, OpenAiModel};
use serde_json::json;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

struct ImageTool;

#[async_trait::async_trait]
impl BaseTool for ImageTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn description(&self) -> &str {
        "image fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type":"object"})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("图片调用必须使用保留 raw 的 invoke_output")
    }
    async fn invoke_output(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ToolOutput::from_raw_result(
            "Image read\n[image: image/png]",
            RawToolResult::new(json!({
                "content": [{"type":"image", "mimeType":"image/png", "data":PNG}]
            })),
        ))
    }
}

/// [回归测试] Chat Completions 下 raw 图片经 dispatch、历史往返和模型转换后仍进入请求。
#[tokio::test]
async fn test_openai_tool_image_survives_dispatch_and_history_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let model = Arc::new(OpenAiModel::new(OpenAiConfig::new(
        "https://fixture.example/v1/".parse().unwrap(),
        "fixture-key",
        "fixture-vision-model",
    )));
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(model.clone())))
    .build();
    ctx.runtime
        .tools
        .write()
        .insert("Read".into(), Arc::new(ImageTool));
    let reasoning = Reasoning::with_tools("", vec![ToolCall::new("read-image", "Read", json!({}))]);
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    dispatch_tools(&ctx, &reasoning, &catalog, &CancellationToken::new())
        .await
        .unwrap();
    let messages = ctx.visible_messages();
    let restored: Vec<BaseMessage> =
        serde_json::from_slice(&serde_json::to_vec(&messages).unwrap()).unwrap();
    assert_eq!(restored.len(), 2, "附件不能伪造持久用户消息");
    assert!(
        restored[1]
            .content_blocks()
            .iter()
            .any(|block| matches!(block,
                ContentBlock::Image { source: ImageSource::Base64 { media_type, data } }
                if media_type == "image/png" && data == PNG
            )),
        "图片不能在 dispatch 或历史恢复阶段丢失"
    );
    let request = ModelRequest::new(AgentModelBridge::convert_messages(&restored).unwrap());
    let prepared = model.prepare_request(&request).unwrap();
    let wire = &prepared.body().as_value()["messages"];
    assert_eq!(wire[1]["role"], "tool");
    assert_eq!(wire[1]["tool_call_id"], "read-image");
    assert!(wire[1]["content"].is_string());
    assert_eq!(wire[2]["role"], "user");
    assert_eq!(
        wire[2]["content"][1]["image_url"]["url"],
        format!("data:image/png;base64,{PNG}")
    );
}
