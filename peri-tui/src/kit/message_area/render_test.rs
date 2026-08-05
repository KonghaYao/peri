//! Tests
use super::*;
use crate::kit::message_area::selection::build_wrap_map;
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider, TuiNoteLevel,
    TuiRenderUnit, TuiSkillPresentation, TuiSubAgentGroup, TuiSystemNote, TuiTodoChange,
    TuiTodoChangeKind, TuiTodoItem, TuiTodoPresentation, TuiTodoStatus, TuiToolCard,
    TuiToolPresentation,
};
use unicode_width::UnicodeWidthStr;

/// 宽度为 1 时，所有 VM 变体的 vm_to_lines 不应 panic。
/// [回归测试] 快速缩小终端到极小宽度时程序崩溃。
#[test]
fn test_vm_to_lines_all_variants_width_1() {
    let empty_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: None,
        content_hash: 42,
    });

    let text_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "hello world\n测试内容".to_string(),
        reasoning: None,
        content_hash: 43,
    });

    let table_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "| col1 | col2 |\n|------|------|\n| a    | b    |\n| c    | d    |".to_string(),
        reasoning: None,
        content_hash: 44,
    });

    let system_note = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "系统通知".to_string(),
        level: TuiNoteLevel::Info,
        content_hash: 45,
    });

    let divider = TuiRenderUnit::TuiDivider(TuiDivider {
        label: None,
        content_hash: 46,
    });

    let collapsed = TuiRenderUnit::TuiCollapsedGroup(TuiCollapsedGroup {
        title: "折叠组标题".to_string(),
        count: 3,
        view_models: vec![],
        content_hash: 47,
    });

    let ask_user = TuiRenderUnit::TuiAskUserBlock(TuiAskUserBlock {
        items: vec![],
        is_error: false,
        content_hash: 48,
    });

    let subagent = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "test-agent".to_string(),
        agent_name: "Test Agent".to_string(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "t1".to_string(),
            tool_name: "Bash".to_string(),
            input_summary: "bash test".to_string(),
            output_summary: String::new(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            diff: None,
            presentation: TuiToolPresentation::Generic,
            content_hash: 100,
            tool_calls_count: 0,
        })]),
        collapsed: false,
        is_running: false,
        content_hash: 49,
    });

    let all_variants: Vec<(&str, TuiRenderUnit)> = vec![
        ("空 AssistantBubble", empty_bubble),
        ("文本 AssistantBubble", text_bubble),
        ("表格 AssistantBubble", table_bubble),
        ("SystemNote", system_note),
        ("Divider", divider),
        ("CollapsedGroup", collapsed),
        ("AskUserBlock", ask_user),
        ("SubAgentGroup", subagent),
    ];

    for width in [1usize, 2, 3, 5] {
        for (_label, vm) in &all_variants {
            let _lines = vm_to_lines(vm, width);
            // 只要不 panic 就算通过
        }
    }
}

/// build_wrap_map 在宽度为 1 时不应 panic（所有字符折为单独行）。
/// [回归测试] 历史消息中长文本在极窄宽度下可能导致 line_count 异常。
#[test]
fn test_build_wrap_map_width_1_no_panic() {
    // 模拟一段典型对话文本（含 CJK）
    let lines = vec![
        ratatui_kit::ratatui::text::Line::from(
            "这是一段包含中英文 mixed content 的代表性消息，模拟真实对话内容。",
        ),
        ratatui_kit::ratatui::text::Line::from("第二行：包含各种字符 hello world 12345 !@#$%"),
        ratatui_kit::ratatui::text::Line::from(
            "Third line: purely ASCII text for comparison purposes.",
        ),
    ];
    for width in [1u16, 2, 3, 5, 10] {
        let _ = build_wrap_map(&lines, width);
    }
}

/// 空 AssistantBubble（无 text、无 reasoning）返回空 lines。
/// [回归测试] 这会导致 build_wrap_map 产出 visual_rows=0，
/// 进而 total_visual_rows 可能为 0，触发 scrollbar position 计算的 usize underflow。
#[test]
fn test_empty_assistant_bubble_returns_zero_lines() {
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: None,
        content_hash: 1,
    });
    let lines = vm_to_lines(&vm, 80);
    assert!(lines.is_empty(), "空 AssistantBubble 应返回 0 行");
}

/// SystemNote 应根据 data.level 渲染不同正文颜色：Info→muted, Warning→warning, Error→error。
#[test]
fn test_system_note_level_colors() {
    use peri_theme::atoms::THEME_ATOM;
    let semantic = THEME_ATOM.state().read().semantic;

    // Info 级别
    let info_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "info message".to_string(),
        level: TuiNoteLevel::Info,
        content_hash: 200,
    });
    let info_lines = vm_to_lines(&info_vm, 80);
    assert!(!info_lines.is_empty());
    assert!(!info_lines[0].spans.is_empty());
    assert_eq!(
        info_lines[0].spans.last().unwrap().style.fg,
        Some(semantic.text.muted),
    );

    // Warning 级别——文字不含 "warning" 关键词，验证颜色来自 level
    let warn_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "deprecation notice".to_string(),
        level: TuiNoteLevel::Warning,
        content_hash: 201,
    });
    let warn_lines = vm_to_lines(&warn_vm, 80);
    assert!(!warn_lines.is_empty() && !warn_lines[0].spans.is_empty());
    assert_eq!(
        warn_lines[0].spans.last().unwrap().style.fg,
        Some(semantic.status.warning),
    );

    // Error 级别——文字不含 "error"/"失败"/❌，验证颜色来自 level
    let err_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "something went wrong".to_string(),
        level: TuiNoteLevel::Error,
        content_hash: 202,
    });
    let err_lines = vm_to_lines(&err_vm, 80);
    assert!(!err_lines.is_empty() && !err_lines[0].spans.is_empty());
    assert_eq!(
        err_lines[0].spans.last().unwrap().style.fg,
        Some(semantic.status.error),
    );
}

/// 验证 render_reasoning_block 的截断逻辑：每条 thinking tail 行在
/// build_wrap_map 中应占恰好 1 个 visual row（不折行）。
#[test]
fn test_reasoning_truncate_no_wrap() {
    use crate::kit::tui_render_unit::TuiReasoningBlock;

    // 构造超长推理文本，验证每条 tail 行被截断到 ≤width
    let long_line = "a".repeat(200);
    let reasoning = TuiReasoningBlock {
        text: long_line,
        collapsed: false,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: Some(reasoning),
        content_hash: 1,
    });

    let mut failures = Vec::new();
    for width in [20u16, 40, 60, 80] {
        let lines = vm_to_lines(&vm, width as usize);
        let (_, wm) = build_wrap_map(&lines, width);

        // 跳过空行 0，header 行 1。验证 tail 行（≥2）
        for logical_idx in 2..lines.len() {
            let info = &wm[logical_idx];
            let rows = info.visual_end - info.visual_start;
            if lines[logical_idx].spans.is_empty() {
                continue;
            }
            let text: String = lines[logical_idx]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            if rows != 1 {
                failures.push(format!(
                    "width={width}: tail line {logical_idx} (\"{text}\") has {rows} visual rows, expected 1"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "截断后仍被 Paragraph 换行:\n{}",
        failures.join("\n")
    );
}

#[test]
fn skill_card_hides_raw_skill_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "skill-1".into(),
        tool_name: "Skill".into(),
        input_summary: "skill: using-superpowers".into(),
        output_summary: "---\nname: using-superpowers\n---\nfull SKILL.md body".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
            name: "using-superpowers".into(),
        }),
        content_hash: 1,
        tool_calls_count: 0,
    };

    let text = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), 80)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("Skill"));
    assert!(text.contains("✓"));
    assert!(text.contains("using-superpowers"));
    assert!(!text.contains("full SKILL.md body"));
    assert!(!text.contains("---"));
}

#[test]
fn todo_card_renders_progress_and_changes_without_raw_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "todo-1".into(),
        tool_name: "TodoWrite".into(),
        input_summary: "todos: 2".into(),
        output_summary: "+[0],[1]".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
            current_items: vec![TuiTodoItem {
                content: "实现语义卡片".into(),
                active_form: None,
                status: TuiTodoStatus::Completed,
            }],
            changes: vec![TuiTodoChange {
                kind: TuiTodoChangeKind::Completed,
                content: "实现语义卡片".into(),
            }],
            is_initial: false,
            completed_count: 1,
            total_count: 1,
        }),
        content_hash: 2,
        tool_calls_count: 0,
    };

    let text = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), 80)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("TodoUpdate"));
    assert!(text.contains("1/1"));
    assert!(text.contains("✓"));
    assert!(text.contains("实现语义卡片"));
    assert!(!text.contains("+[0],[1]"));
}

#[test]
fn semantic_tool_card_respects_narrow_display_width() {
    crate::i18n::init(Some("zh-CN"));
    let card = TuiToolCard {
        tool_id: "todo-narrow".into(),
        tool_name: "TodoWrite".into(),
        input_summary: String::new(),
        output_summary: "+[0]".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
            current_items: vec![],
            changes: vec![TuiTodoChange {
                kind: TuiTodoChangeKind::Added,
                content: "这是一个足够长的任务标题，用于验证窄终端截断行为".into(),
            }],
            is_initial: true,
            completed_count: 0,
            total_count: 1,
        }),
        content_hash: 3,
        tool_calls_count: 0,
    };
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), 12);
    for width in 1..=4 {
        let narrow_lines = vm_to_lines(
            &TuiRenderUnit::TuiToolCard(TuiToolCard {
                tool_id: format!("todo-narrow-{width}"),
                tool_name: "TodoWrite".into(),
                input_summary: String::new(),
                output_summary: String::new(),
                is_error: false,
                is_running: false,
                running_duration_ms: None,
                diff: None,
                presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
                    current_items: vec![],
                    changes: vec![],
                    is_initial: true,
                    completed_count: 0,
                    total_count: 1,
                }),
                content_hash: width as u64,
                tool_calls_count: 0,
            }),
            width,
        );
        assert!(narrow_lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| span.content.width())
                .sum::<usize>()
                <= width
        }));
    }
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>()
            <= 12
    }));
}

#[test]
fn skill_card_hides_raw_output_when_name_is_missing_or_call_failed() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "skill-failed".into(),
        tool_name: "Skill".into(),
        input_summary: String::new(),
        output_summary: "---\nname: secret-skill\n---\nfull SKILL.md body".into(),
        is_error: true,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
            name: "unknown".into(),
        }),
        content_hash: 4,
        tool_calls_count: 0,
    };

    let text = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), 80)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("✗"));
    assert!(!text.contains("full SKILL.md body"));
    assert!(!text.contains("---"));
}

#[test]
fn failed_semantic_cards_respect_extreme_narrow_widths() {
    crate::i18n::init(Some("en"));
    for width in 1..=4 {
        let card = TuiToolCard {
            tool_id: format!("skill-error-{width}"),
            tool_name: "Skill".into(),
            input_summary: "long raw input".into(),
            output_summary: "long raw SKILL.md body".into(),
            is_error: true,
            is_running: false,
            running_duration_ms: None,
            diff: None,
            presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
                name: "using-superpowers".into(),
            }),
            content_hash: width as u64,
            tool_calls_count: 0,
        };
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), width);
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| span.content.width())
                .sum::<usize>()
                <= width
        }));
    }
}

#[test]
fn nested_semantic_cards_respect_extreme_narrow_widths() {
    crate::i18n::init(Some("en"));
    for is_error in [false, true] {
        for width in 1..=4 {
            let card = TuiToolCard {
                tool_id: format!("nested-skill-{is_error}-{width}"),
                tool_name: "Skill".into(),
                input_summary: "raw input".into(),
                output_summary: "raw SKILL.md output".into(),
                is_error,
                is_running: false,
                running_duration_ms: None,
                diff: None,
                presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
                    name: "using-superpowers".into(),
                }),
                content_hash: width as u64,
                tool_calls_count: 0,
            };
            let group = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
                agent_id: "nested-agent".into(),
                agent_name: "nested".into(),
                view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(card)]),
                collapsed: false,
                is_running: false,
                content_hash: width as u64,
            });
            let lines = vm_to_lines(&group, width);
            assert!(lines.iter().all(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.width())
                    .sum::<usize>()
                    <= width
            }));
        }
    }
}

// ── md 复制按钮 ────────────────────────────────────────────────────────────

fn assistant_bubble_with_text(text: &str, hash: u64) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: text.to_string(),
        reasoning: None,
        content_hash: hash,
    })
}

/// 顶层渲染（render_copy_button=true）时，超过 MD_COPY_MIN_CHARS（400）字符的
/// AssistantBubble 末尾追加复制按钮行，并返回按钮布局信息（逻辑索引 = 末行，
/// 列范围 = 按钮文本）。
#[test]
fn test_assistant_bubble_renders_copy_button() {
    crate::i18n::init(Some("en"));
    // 401 字符 > 400 阈值
    let vm = assistant_bubble_with_text(&"x".repeat(401), 1);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn) = super::vm_to_lines_cached(&vm, 80, &mut cache, true);

    let btn_line = lines.last().unwrap();
    let text: String = btn_line.spans.iter().map(|s| s.content.as_ref()).collect();
    let btn_width = 2 + crate::i18n::tr("msg-copy-md").width();
    assert_eq!(
        text,
        format!("{}{}", " ".repeat(80 - btn_width), " Copy "),
        "按钮行 = 前导填充空格（右对齐）+ 左右各 1 空格 + i18n 按钮文本"
    );

    let btn = btn.expect("超过 400 字符的 AssistantBubble 应返回按钮布局");
    assert_eq!(btn.logical_idx, lines.len() - 1, "按钮行是最后一逻辑行");
    assert_eq!(
        btn.x_start,
        (80 - btn_width) as u16,
        "点击区域 = 反色块本身（右对齐，不含前导填充）"
    );
    assert_eq!(btn.x_end, 80, "x_end = 行尾（左空格 + 文本 + 右空格）");
}

/// 宽度不足时按钮行会折行 → 不渲染按钮（也不返回布局），避免点击区域错位。
#[test]
fn test_copy_button_hidden_when_narrow() {
    crate::i18n::init(Some("en"));
    // 文本超过阈值（401 字符），但视宽 3 容纳不下按钮行（" Copy " 6 列）
    let vm = assistant_bubble_with_text(&"x".repeat(401), 2);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn) = super::vm_to_lines_cached(&vm, 3, &mut cache, true);

    assert!(btn.is_none(), "宽度不足时不应返回按钮布局");
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(!text.contains("Copy"), "宽度不足时不应渲染按钮行");
}

/// 空文本不渲染按钮（没有可复制的内容）。
#[test]
fn test_copy_button_hidden_for_empty_text() {
    let vm = assistant_bubble_with_text("", 3);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn) = super::vm_to_lines_cached(&vm, 80, &mut cache, true);

    assert!(lines.is_empty());
    assert!(btn.is_none());
}

/// UserBubble 不渲染复制按钮（仅 AI 回复）。
#[test]
fn test_copy_button_hidden_for_user_bubble() {
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble {
        text: "my input".to_string(),
        content_hash: 4,
        reminder: None,
    });
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn) = super::vm_to_lines_cached(&vm, 80, &mut cache, true);

    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(!text.contains("Copy"), "UserBubble 不应包含复制按钮文本");
    assert!(btn.is_none());
}

/// 嵌套渲染（vm_to_lines，render_copy_button=false，SubAgentGroup 递归路径）
/// 不渲染复制按钮——嵌套消息无屏幕坐标映射，避免"看起来可点但点了没反应"。
#[test]
fn test_copy_button_hidden_in_nested_render() {
    crate::i18n::init(Some("en"));
    // 文本超过阈值，但 render_copy_button=false → 仍不渲染
    let vm = assistant_bubble_with_text(&"x".repeat(401), 5);
    let lines = super::vm_to_lines(&vm, 80);

    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(!text.contains("Copy"), "嵌套渲染不应包含复制按钮行");
}

/// 短文本（≤400 字符，含恰好 400 的边界）不渲染复制按钮——避免每条消息
/// 都出现按钮行造成视觉噪音（短消息直接选中复制即可）。
#[test]
fn test_copy_button_hidden_for_short_text() {
    crate::i18n::init(Some("en"));
    for text in ["hello world".to_string(), "x".repeat(400)] {
        let vm = assistant_bubble_with_text(&text, 6);
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        let (lines, btn) = super::vm_to_lines_cached(&vm, 80, &mut cache, true);

        assert!(btn.is_none(), "≤200 字符不应返回按钮布局");
        let rendered: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!rendered.contains("Copy"), "≤200 字符不应渲染按钮行");
    }
}
