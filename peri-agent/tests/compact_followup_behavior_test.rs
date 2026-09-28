//! PR #175 追加审查：公开估算入口的媒体及工具调用表示边界。
//! 固定 base64 字符串只测预算，不声称由这里验证媒体解码或 provider 成本。
use async_trait::async_trait;
use peri_agent::{
    agent::react::{ReactLLM, Reasoning, StreamingContext},
    error::AgentResult,
    messages::{BaseMessage, ContentBlock, DocumentSource, MessageContent, ToolCallRequest},
    tools::BaseTool,
};

struct BudgetProbe;

#[async_trait]
impl ReactLLM for BudgetProbe {
    async fn generate_reasoning(
        &self,
        _: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        unreachable!("预算估算不得调用模型")
    }
}

fn estimate(message: BaseMessage) -> u64 {
    BudgetProbe.estimate_request_tokens(&[message], &[])
}

fn tool_block(id: &str, count: usize) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: "Write".into(),
        input: serde_json::json!({"content": "x".repeat(count)}),
    }
}

fn nested_attachment(count: usize) -> ContentBlock {
    ContentBlock::ToolResult {
        id: None,
        tool_use_id: "attachment".into(),
        is_error: false,
        content: vec![
            ContentBlock::image_base64("image/png", "A".repeat(count)),
            ContentBlock::Document {
                source: DocumentSource::Base64 {
                    media_type: "application/pdf".into(),
                    data: "B".repeat(count),
                },
                title: None,
            },
        ],
    }
}

/// [回归测试] 工具结果嵌套媒体与 Raw 的已知媒体块同样不能按 base64 长度计预算。
#[test]
fn test_followup_nested_and_raw_media_cost_does_not_scale_with_base64() {
    let small = nested_attachment(40);
    let large = nested_attachment(800_000);
    let typed_small = estimate(BaseMessage::human(MessageContent::Blocks(vec![small])));
    let typed_large = estimate(BaseMessage::human(MessageContent::Blocks(vec![
        large.clone()
    ])));
    let raw_large = estimate(BaseMessage::human(MessageContent::Raw(vec![
        serde_json::to_value(large).unwrap(),
    ])));
    assert_eq!(typed_small, typed_large);
    assert_eq!(typed_large, raw_large);
    assert!(
        typed_large < 100_000,
        "附件字节大小不能独自造成高压：{typed_large}"
    );
}

/// [负对照] 文本型 Document 的真实文字无论 typed/raw/nested 都须计入预算。
#[test]
fn test_followup_text_documents_remain_budgeted_in_all_supported_shapes() {
    let block = ContentBlock::Document {
        source: DocumentSource::Text {
            text: "text".repeat(110_000),
        },
        title: Some("report".into()),
    };
    let typed = estimate(BaseMessage::human(MessageContent::Blocks(vec![
        block.clone()
    ])));
    let raw = estimate(BaseMessage::human(MessageContent::Raw(vec![
        serde_json::to_value(&block).unwrap(),
    ])));
    let nested = estimate(BaseMessage::tool_result(
        "doc",
        MessageContent::Blocks(vec![ContentBlock::ToolResult {
            id: None,
            tool_use_id: "doc".into(),
            is_error: false,
            content: vec![block],
        }]),
    ));
    assert!(typed >= 110_000, "文本附件不能被媒体占位吞掉：{typed}");
    assert_eq!(raw, typed);
    assert_eq!(nested, typed);
}

/// [回归测试] URI scheme 不区分 ASCII 大小写；原样 URL 构造不能绕回 base64 字符计量。
#[test]
fn test_followup_data_uri_scheme_case_preserves_bounded_media_budget() {
    let body = "A".repeat(800_000);
    let lowercase = estimate(BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::image_url(format!("data:image/png;base64,{body}")),
    ])));
    for scheme in ["DATA", "Data", "dAtA"] {
        let url = format!("{scheme}:image/png;base64,{body}");
        assert_eq!(url::Url::parse(&url).unwrap().scheme(), "data");
        let image = estimate(BaseMessage::human(MessageContent::Blocks(vec![
            ContentBlock::image_url(&url),
        ])));
        let document = estimate(BaseMessage::human(MessageContent::Blocks(vec![
            ContentBlock::Document {
                source: DocumentSource::Url { url },
                title: None,
            },
        ])));
        assert_eq!(image, lowercase, "大小写 scheme 改变了图片预算");
        assert_eq!(document, lowercase, "大小写 scheme 改变了文档预算");
    }
}

/// [回归测试] ai(content) 不创建 canonical tool_calls，不能把其 block 当作已计量镜像。
#[test]
fn test_followup_ai_tool_blocks_without_canonical_mirror_are_budgeted() {
    let block = tool_block("write", 440_000);
    let message = BaseMessage::ai(MessageContent::Blocks(vec![block.clone()]));
    let restored: BaseMessage =
        serde_json::from_value(serde_json::to_value(message).unwrap()).unwrap();
    assert!(matches!(&restored, BaseMessage::Ai { tool_calls, .. } if tool_calls.is_empty()));
    assert!(
        estimate(restored) >= 110_000,
        "恢复不会自动补 canonical 镜像，巨型工具参数必须计量"
    );
    // Raw 估算形状也需一致；当前生产 AgentModelBridge 明确拒绝 Raw，不将其当作 provider 支持证明。
    assert!(
        estimate(BaseMessage::ai(MessageContent::Raw(vec![
            serde_json::to_value(block).unwrap()
        ]))) >= 110_000
    );
}

/// [负对照] ai_from_blocks 已创建相同调用的镜像，输入参数应恰好计量一次。
#[test]
fn test_followup_canonical_tool_mirror_is_counted_once() {
    let block = tool_block("write", 440_000);
    let without_mirror = estimate(BaseMessage::ai(MessageContent::Blocks(vec![block.clone()])));
    let with_mirror = estimate(BaseMessage::ai_from_blocks(vec![block]));
    assert!(with_mirror >= 110_000);
    assert_eq!(with_mirror, without_mirror);
}

/// [回归测试] 只有部分调用存在 canonical 镜像时，不能漏掉其他 block 参数。
#[test]
fn test_followup_partial_tool_mirror_does_not_hide_other_arguments() {
    let small = tool_block("small", 4);
    let large = tool_block("large", 440_000);
    let message = BaseMessage::ai_with_tool_calls(
        MessageContent::Blocks(vec![small, large]),
        vec![ToolCallRequest::new(
            "small",
            "Write",
            serde_json::json!({"content":"xxxx"}),
        )],
    );
    assert!(
        estimate(message) >= 110_000,
        "不能因首个 canonical 调用存在就跳过其它调用"
    );
}

/// [负对照] 未知 JSON 的 data 字段未声明媒体契约，应继续作为真实文本计量。
#[test]
fn test_followup_unknown_json_data_is_not_treated_as_binary_media() {
    let message = BaseMessage::human(MessageContent::Blocks(vec![ContentBlock::Unknown(
        serde_json::json!({"type":"custom_text", "data":"text".repeat(110_000)}),
    )]));
    assert!(estimate(message) >= 110_000);
}
