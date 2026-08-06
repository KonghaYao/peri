//! execute_prediction facade 单元测试。
//!
//! 覆盖 [`extract_prediction_text`]——Prediction 文本提取纯函数，是
//! [`execute_prediction`] 唯一可独立单测的逻辑（agent 构建需要真实 LLM）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数）。

use peri_acp_types::event_data::PredictionAction;
use peri_acp_types::messages::BaseMessage;

use super::{extract_prediction_text, parse_prediction_actions};

// ── extract_prediction_text: 路径分支测试 ───────────────────────────────────

/// 正常路径：返回最后一条非空 AI 消息文本（两侧空白被裁剪）
#[test]
fn test_extract_prediction_text_返回最后一条_ai_消息() {
    // Arrange
    let messages = vec![
        BaseMessage::human("用户提问"),
        BaseMessage::ai("  第一条回答  "),
        BaseMessage::human("追问"),
        BaseMessage::ai("  最终预测文本  "),
    ];

    // Act
    let text = extract_prediction_text(&messages);

    // Assert
    assert_eq!(text, "最终预测文本");
}

/// 跳过空 AI 消息：返回更早的非空 AI 消息
#[test]
fn test_extract_prediction_text_跳过空_ai_消息() {
    // Arrange
    let messages = vec![
        BaseMessage::ai("  有效预测  "),
        BaseMessage::ai("   "),
        BaseMessage::ai(""),
    ];

    // Act
    let text = extract_prediction_text(&messages);

    // Assert
    assert_eq!(text, "有效预测");
}

/// 无 AI 消息：返回空字符串
#[test]
fn test_extract_prediction_text_无_ai_消息返回空() {
    // Arrange
    let messages = vec![
        BaseMessage::human("只有用户消息"),
        BaseMessage::system("系统消息"),
    ];

    // Act
    let text = extract_prediction_text(&messages);

    // Assert
    assert!(text.is_empty(), "无 AI 消息时应返回空字符串");
}

/// 全部 AI 消息为空：返回空字符串
#[test]
fn test_extract_prediction_text_全部_ai_为空返回空() {
    // Arrange
    let messages = vec![
        BaseMessage::ai(""),
        BaseMessage::ai("   "),
        BaseMessage::ai("\n\t"),
    ];

    // Act
    let text = extract_prediction_text(&messages);

    // Assert
    assert!(text.is_empty(), "全部 AI 消息为空时应返回空字符串");
}

/// 空消息列表：返回空字符串
#[test]
fn test_extract_prediction_text_空列表返回空() {
    // Arrange
    let messages: Vec<BaseMessage> = vec![];

    // Act
    let text = extract_prediction_text(&messages);

    // Assert
    assert!(text.is_empty(), "空消息列表应返回空字符串");
}

// ── parse_prediction_actions: 标记解析测试 ──────────────────────────────────

/// 纯文本无标记：回落为单个 Placeholder
#[test]
fn test_parse_纯文本回落_placeholder() {
    let actions = parse_prediction_actions("继续修 bug");
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        PredictionAction::Placeholder { text } if text == "继续修 bug"
    ));
}

/// 单个 title 标记
#[test]
fn test_parse_title标记() {
    let actions = parse_prediction_actions("<peri:title>修复认证模块</peri:title>");
    assert!(matches!(
        &actions[0],
        PredictionAction::SetTitle { title } if title == "修复认证模块"
    ));
}

/// 混合：占位文本 + 三种标记，顺序为占位优先
#[test]
fn test_parse_混合文本与标记() {
    let actions = parse_prediction_actions(
        "继续排查内存泄漏 <peri:title>排查内存泄漏</peri:title>\
         <peri:tag>bugfix</peri:tag><peri:summary>定位到缓存未释放</peri:summary>",
    );
    assert_eq!(actions.len(), 4);
    assert!(
        matches!(&actions[0], PredictionAction::Placeholder { text } if text == "继续排查内存泄漏")
    );
    assert!(matches!(&actions[1], PredictionAction::SetTitle { .. }));
    assert!(matches!(&actions[2], PredictionAction::AddTag { tag } if tag == "bugfix"));
    assert!(
        matches!(&actions[3], PredictionAction::Summary { text } if text == "定位到缓存未释放")
    );
}

/// 未知标签忽略，内容并入占位文本
#[test]
fn test_parse_未知标签忽略() {
    let actions = parse_prediction_actions("先看看 <peri:unknown>内部内容</peri:unknown> 再改");
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        PredictionAction::Placeholder { text } if text == "先看看 内部内容 再改"
    ));
}

/// 同名动作重复：后者覆盖前者
#[test]
fn test_parse_重复动作后者覆盖() {
    let actions =
        parse_prediction_actions("<peri:title>旧标题</peri:title><peri:title>新标题</peri:title>");
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        PredictionAction::SetTitle { title } if title == "新标题"
    ));
}

/// 嵌套注入：非贪婪匹配第一个闭合，内嵌标签作为内容吞掉
#[test]
fn test_parse_嵌套标记按内容处理() {
    let actions = parse_prediction_actions("<peri:title>a<peri:tag>b</peri:title>");
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        PredictionAction::SetTitle { title } if title == "a<peri:tag>b"
    ));
}

/// 超长内容截断到 200 字符
#[test]
fn test_parse_超长内容截断() {
    let long = "x".repeat(300);
    let actions = parse_prediction_actions(&format!("<peri:summary>{long}</peri:summary>"));
    assert!(matches!(
        &actions[0],
        PredictionAction::Summary { text } if text.chars().count() == 200
    ));
}

/// 控制字符（含换行）剥离
#[test]
fn test_parse_控制字符剥离() {
    let actions = parse_prediction_actions("<peri:title>第一行\n第二行\t</peri:title>");
    assert!(matches!(
        &actions[0],
        PredictionAction::SetTitle { title } if title == "第一行第二行"
    ));
}

/// 空内容动作跳过
#[test]
fn test_parse_空内容动作跳过() {
    let actions = parse_prediction_actions("<peri:title></peri:title>");
    assert!(actions.is_empty(), "空内容动作应被跳过");
}

/// 空输入返回空列表
#[test]
fn test_parse_空输入返回空() {
    let actions = parse_prediction_actions("");
    assert!(actions.is_empty());
}

/// 未闭合标签按纯文本处理
#[test]
fn test_parse_未闭合标签按纯文本() {
    let actions = parse_prediction_actions("试试 <peri:title>没有闭合");
    assert_eq!(actions.len(), 1);
    assert!(matches!(&actions[0], PredictionAction::Placeholder { .. }));
}
