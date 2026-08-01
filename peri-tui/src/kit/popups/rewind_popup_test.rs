//! Tests

use super::*;
use crate::i18n;
use crate::kit::atoms::{RewindBudgetState, RewindFileChange};
use fluent_bundle::FluentValue;
use peri_acp_types::event_data::{RewindMessage, RewindPreview};
use serial_test::serial;
use std::sync::Arc;

fn theme() -> Arc<ThemeDefinition> {
    peri_theme::atoms::THEME_ATOM.state().read().clone()
}

/// 候选视图：2 条 user 候选 + 选中标记（渲染行构造纯函数 build_popup_lines）。
#[test]
#[serial]
fn test_popup_lines_candidates_view() {
    crate::kit::atoms::init_atoms();
    let preview = Some(RewindPreview {
        files: vec![],
        messages: vec![
            RewindMessage {
                id: "m1".into(),
                role: "user".into(),
                preview: "第一轮问题".into(),
            },
            RewindMessage {
                id: "m2".into(),
                role: "user".into(),
                preview: "第二轮问题".into(),
            },
        ],
    });
    let lines = build_popup_lines(&preview, &RewindBudgetState::Idle, &None, 0, 0, &theme());
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // 动态期望（测试环境默认 en）——严禁写死中文
    assert!(
        text.contains(&i18n::tr_args(
            "rewind-title-count",
            &[("count".into(), FluentValue::from(2i64))]
        )),
        "候选视图显示数量"
    );
    assert!(
        text.contains("第一轮问题") && text.contains("第二轮问题"),
        "渲染全部候选"
    );
    assert!(text.contains(">"), "选中行有标记");
}

/// 候选视图：空候选显示"无可回退"。
#[test]
#[serial]
fn test_popup_lines_empty_candidates() {
    crate::kit::atoms::init_atoms();
    let lines = build_popup_lines(
        &Some(RewindPreview {
            files: vec![],
            messages: vec![],
        }),
        &RewindBudgetState::Idle,
        &None,
        0,
        0,
        &theme(),
    );
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&i18n::tr("rewind-empty")), "空候选提示");
}

/// 查询失败：显示错误文案。
#[test]
#[serial]
fn test_popup_lines_query_error() {
    crate::kit::atoms::init_atoms();
    let lines = build_popup_lines(
        &None,
        &RewindBudgetState::Idle,
        &Some("RPC timeout".to_string()),
        0,
        0,
        &theme(),
    );
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(&i18n::tr_args(
            "rewind-query-failed",
            &[("error".into(), FluentValue::from("RPC timeout"))]
        )),
        "错误文案透出"
    );
}

/// 候选未返回（preview=None 且无错误）：显示加载中。
#[test]
#[serial]
fn test_popup_lines_loading() {
    crate::kit::atoms::init_atoms();
    let lines = build_popup_lines(&None, &RewindBudgetState::Idle, &None, 0, 0, &theme());
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&i18n::tr("rewind-loading")), "加载中提示");
}

/// 预算视图：文件列表 + 确认提示。
#[test]
#[serial]
fn test_popup_lines_budget_view() {
    crate::kit::atoms::init_atoms();
    let budget = RewindBudgetState::Files(vec![
        RewindFileChange {
            path: "src/main.rs".into(),
            kind: "edit".into(),
        },
        RewindFileChange {
            path: "new_file.txt".into(),
            kind: "write".into(),
        },
    ]);
    let lines = build_popup_lines(&None, &budget, &None, 0, 0, &theme());
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(&i18n::tr_args(
            "rewind-budget-title",
            &[("count".into(), FluentValue::from(2i64))]
        )),
        "预算数量"
    );
    assert!(
        text.contains("[edit] src/main.rs") && text.contains("[write] new_file.txt"),
        "文件列表"
    );
    assert!(
        text.contains(&i18n::tr("rewind-budget-confirm-hint")),
        "确认提示"
    );
}

/// 执行中：显示"正在回退"。
#[test]
#[serial]
fn test_popup_lines_executing() {
    crate::kit::atoms::init_atoms();
    let lines = build_popup_lines(&None, &RewindBudgetState::Executing, &None, 0, 0, &theme());
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&i18n::tr("rewind-executing")), "执行中提示");
}

#[test]
fn test_truncate_str_short() {
    assert_eq!(truncate_str("hello", 10), "hello");
}

#[test]
fn test_truncate_str_exact() {
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn test_truncate_str_long() {
    assert_eq!(truncate_str("hello world", 5), "hello…");
}

#[test]
fn test_truncate_str_cjk() {
    // 中文字符 1 char = 3 bytes；chars().take 计 char 数不 panic
    assert_eq!(truncate_str("你好世界朋友", 4), "你好世界…");
}

#[test]
fn test_rewind_view_states_distinct() {
    assert_ne!(RewindView::Candidates, RewindView::Budget);
    assert_ne!(RewindView::Budget, RewindView::Executing);
    assert_ne!(RewindView::Executing, RewindView::Candidates);
}
