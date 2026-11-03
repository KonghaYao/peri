//! Edit 行语法的纯布局回归：列对齐、宽度上限、长值与光标窗口、窄屏降级。

use super::*;
use crate::kit::panels::login::render::LoginRenderColors;
use ratatui_kit::ratatui::text::Line;
use serial_test::serial;
use unicode_width::UnicodeWidthStr;

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

fn colors() -> LoginRenderColors {
    LoginRenderColors::reset()
}

fn with_lang(lang: &str, scenario: impl FnOnce()) {
    crate::i18n::init(Some(lang));
    scenario();
    crate::i18n::init(Some("en"));
}

#[test]
#[serial]
fn metrics_align_labels_by_current_language() {
    with_lang("zh-CN", || {
        let metrics = EditViewMetrics::for_width(100);
        assert_eq!(metrics.content_width, 99);
        let expected_label_col = FIELD_LABEL_KEYS
            .iter()
            .map(|key| crate::i18n::tr(key).width())
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_COL);
        assert_eq!(
            metrics.label_col, expected_label_col,
            "标签列按当前语言全部标签测量"
        );
        assert_eq!(metrics.value_col, INDENT + 2 + metrics.label_col + 2);
        assert_eq!(metrics.value_budget, 99 - metrics.value_col);
        assert!(!metrics.narrow());
        for width in [80u16, 100, 120] {
            let metrics = EditViewMetrics::for_width(width);
            assert_eq!(
                metrics.value_col,
                INDENT + 2 + expected_label_col + 2,
                "width={width} 值列必须稳定"
            );
            assert!(metrics.value_budget >= 51);
        }
    });
    with_lang("en", || {
        let metrics = EditViewMetrics::for_width(100);
        let expected_label_col = FIELD_LABEL_KEYS
            .iter()
            .map(|key| crate::i18n::tr(key).width())
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_COL);
        assert_eq!(metrics.label_col, expected_label_col);
        assert!(metrics.label_col <= MAX_LABEL_COL);
        assert_eq!(metrics.value_col, INDENT + 2 + metrics.label_col + 2);
        assert_eq!(metrics.value_budget, 99 - metrics.value_col);
    });
}

#[test]
#[serial]
fn field_values_start_in_the_same_column() {
    with_lang("zh-CN", || {
        let metrics = EditViewMetrics::for_width(100);
        let color = colors();
        for (label, value) in [
            ("名称", "gpt"),
            ("类型", "[OpenAI Responses]"),
            ("认证方式", "[ChatGPT plan]"),
        ] {
            let lines =
                text_field_lines(&metrics, label, value, value.chars().count(), false, &color);
            assert_eq!(lines.len(), 1);
            let prefix_width: usize = lines[0].spans[..4]
                .iter()
                .map(|span| span.content.width())
                .sum();
            assert_eq!(
                prefix_width, metrics.value_col,
                "标签 {label} 的值列必须对齐"
            );
        }
    });
}

#[test]
#[serial]
fn every_line_fits_the_content_width() {
    with_lang("zh-CN", || {
        let color = colors();
        for width in [80u16, 100, 120] {
            let metrics = EditViewMetrics::for_width(width);
            let budget = metrics.content_width;
            let mut lines = Vec::new();
            lines.push(title_line(&metrics, "编辑 Provider：very-long-id", &color));
            lines.push(section_line(&metrics, "基本信息", None, &color));
            lines.push(section_line(
                &metrics,
                "高级",
                Some("只读".to_string()),
                &color,
            ));
            lines.extend(text_field_lines(
                &metrics,
                "名称",
                &"p".repeat(300),
                0,
                false,
                &color,
            ));
            lines.extend(choice_field_lines(
                &metrics,
                "类型",
                "[OpenAI Responses]".to_string(),
                Some("←/→/Space: 切换".to_string()),
                false,
                &color,
            ));
            lines.extend(readonly_kv_lines(
                &metrics,
                "基础 URL",
                peri_config::provider::DEFAULT_RESPONSES_URL,
                Some("官方 endpoint，已锁定"),
                &color,
            ));
            lines.push(continuation_line(
                &metrics,
                "已保存的类型，按 Enter 更改",
                &color,
            ));
            lines.extend(status_lines(
                &metrics,
                "登录状态",
                &[StatusEntry::new(
                    "已登录：user@example.com",
                    StatusTone::Success,
                )],
                &color,
            ));
            let (actions_line, _) = action_row(
                &metrics,
                &[
                    "登录 ChatGPT".to_string(),
                    "查询状态".to_string(),
                    "使用此账号".to_string(),
                    "注销".to_string(),
                ],
                Some(3),
                true,
                &color,
            );
            lines.push(actions_line);
            lines.push(button_line(&metrics, "确认", true, &color));
            lines.extend(warning_lines(
                &metrics,
                "ChatGPT 凭据已保存，但 Provider 配置尚未保存",
                &color,
            ));
            lines.extend(error_lines(
                &metrics,
                "Provider 校验失败：一个很长的错误说明",
                &color,
            ));
            for (index, line) in lines.iter().enumerate() {
                assert!(
                    line_width(line) <= budget,
                    "width={width} row={index} 超宽: {} > {budget}\n{:?}",
                    line_width(line),
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                );
            }
        }
    });
}

#[test]
#[serial]
fn long_readonly_path_is_middle_truncated() {
    with_lang("zh-CN", || {
        let color = colors();
        let metrics = EditViewMetrics::for_width(80);
        let path = format!("/Users/example/{}/credentials/chatgpt", "a".repeat(260));
        let lines = readonly_kv_lines(&metrics, "凭据目录", &path, None, &color);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains('…'), "长路径必须中间截断: {text}");
        assert!(text.contains("/Users/example/"), "保留头部: {text}");
        assert!(text.ends_with("credentials/chatgpt"), "保留尾部: {text}");
        assert!(line_width(&lines[0]) <= metrics.content_width);
    });
}

#[test]
#[serial]
fn cursor_window_keeps_the_cursor_inside_for_ascii_and_cjk() {
    let color = colors();
    let budget = 20;
    for value in ["p".repeat(300), "长".repeat(150)] {
        for cursor in [0, 1, 150, value.chars().count()] {
            let window = cursor_window(&value, cursor, budget);
            assert!(window.start <= window.cursor, "start 必须不晚于光标");
            assert!(
                window.cursor < window.end
                    || (window.cursor == value.chars().count()
                        && window.end == value.chars().count()),
                "cursor={cursor} window={window:?}"
            );
            let spans = value_spans(&value, cursor, budget, true, &color);
            let width: usize = spans.iter().map(|span| span.content.width()).sum();
            assert!(width <= budget, "窗口宽度 {width} 超过 {budget}");
            let visible: String = spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(
                visible.contains('‹') || window.start == 0,
                "左侧被裁时必须显示 ‹: {visible}"
            );
            assert!(
                visible.contains('›') || window.end == value.chars().count(),
                "右侧被裁时必须显示 ›: {visible}"
            );
        }
    }
}

#[test]
#[serial]
fn empty_editable_value_has_placeholder_and_no_dangling_colon() {
    with_lang("zh-CN", || {
        let color = colors();
        let metrics = EditViewMetrics::for_width(100);
        let lines = text_field_lines(&metrics, "Fable 模型", "", 0, false, &color);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(&crate::i18n::tr("login-no-model")), "{text}");
        assert!(!text.contains("模型:"), "不得出现悬空冒号: {text}");
    });
}

#[test]
#[serial]
fn narrow_layout_stacks_label_and_value() {
    with_lang("zh-CN", || {
        let color = colors();
        let metrics = EditViewMetrics::for_width(30);
        assert!(metrics.narrow(), "30 列必须降级");
        let lines = text_field_lines(&metrics, "名称", "gpt", 0, true, &color);
        assert_eq!(lines.len(), 2, "窄屏字段占两行");
        for line in &lines {
            assert!(line_width(line) <= metrics.content_width);
        }
        assert!(
            lines[1].spans.iter().any(|span| span.content.contains('g')),
            "第二行显示值"
        );
    });
}

#[test]
#[serial]
fn long_email_status_is_limited_to_two_lines() {
    with_lang("zh-CN", || {
        let color = colors();
        let metrics = EditViewMetrics::for_width(80);
        let email = format!("{}@example.com", "x".repeat(200));
        let entries = [StatusEntry::limited(
            crate::i18n::tr_args(
                "login-auth-signed-in",
                &[(
                    "identity".to_string(),
                    fluent_bundle::FluentValue::from(email.as_str()),
                )],
            ),
            StatusTone::Success,
            2,
        )];
        let lines = status_lines(&metrics, "登录状态", &entries, &color);
        assert_eq!(lines.len(), 2, "状态点只出现一次，长 email 最多 2 行");
        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains('…'), "第三行起必须截断: {text}");
        assert_eq!(text.matches('●').count(), 1, "状态点只出现一次: {text}");
    });
}

#[test]
#[serial]
fn cjk_values_never_split_a_wide_character() {
    with_lang("zh-CN", || {
        let color = colors();
        let metrics = EditViewMetrics::for_width(80);
        let value = "中文".repeat(40);
        let lines = text_field_lines(&metrics, "名称", &value, 0, false, &color);
        assert!(line_width(&lines[0]) <= metrics.content_width);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.ends_with('…'), "截断必须补省略号: {text}");
    });
}
