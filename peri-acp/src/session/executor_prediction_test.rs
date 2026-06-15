//! execute_prediction facade 单元测试。
//!
//! 覆盖 [`extract_prediction_text`]——Prediction 文本提取纯函数，是
//! [`execute_prediction`] 唯一可独立单测的逻辑（agent 构建需要真实 LLM）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数）。

use peri_agent::messages::BaseMessage;

use super::extract_prediction_text;

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
