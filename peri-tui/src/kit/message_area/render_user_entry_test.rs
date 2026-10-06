use super::*;

// ── User entry（§6.1）──────────────────────────────────────────────────

/// 长 prompt 最多 6 个视觉行 + `… +N lines`；无全宽背景色。
#[test]
fn test_user_long_prompt_capped_at_six_lines() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let text = (0..12)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(text));
    let lines = vm_to_lines(&vm, &grid);

    // 1 空行 + 6 正文 + 1 `… +6 lines` + 1 尾部空行（无 role label 行）
    assert_eq!(lines.len(), 9, "6 行上限 + 省略行 + 尾部空行，其余截断");
    assert!(
        lines.last().is_some_and(is_rail_blank),
        "尾部空行（turn 节拍对称，带竖线前缀）"
    );
    let last = line_text(&lines[lines.len() - 2]);
    assert!(
        last.contains("+6 lines"),
        "省略行格式 `… +N lines`，实际 {last:?}"
    );
    assert!(!all_text(&lines).contains("line 9"), "第 7 行起应被截断");

    // 无气泡背景：所有 span bg 均为 None
    for line in &lines {
        for span in &line.spans {
            assert!(span.style.bg.is_none(), "用户消息不应有全宽背景");
        }
    }
    // 不再使用 ❯（§6.1 移除）
    assert!(!all_text(&lines).contains('\u{276f}'));
}

/// slash/@ 局部强调（§6.1）：token 用 accent.user。
#[test]
fn test_user_slash_at_emphasis() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        "run /build and ping @matt".into(),
    ));
    let lines = vm_to_lines(&vm, &grid);
    let body = &lines[1];
    let emphasized = body
        .spans
        .iter()
        .filter(|s| s.style.fg == Some(sem.accents.user))
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        emphasized.contains("/build") && emphasized.contains("@matt"),
        "slash/@ token 应局部强调，实际强调内容: {emphasized:?}"
    );
}

// ── System Reminder 折叠展示 ───────────────────────────────────────────────

#[test]
fn test_system_reminder_collapsed_shows_only_muted_header() {
    let reminder = TuiSystemReminder::legacy("sensitive reminder body".into());
    let unit = TuiRenderUnit::TuiSystemReminder(reminder);
    let grid = GridSpec::with_content(80);
    let lines = vm_to_lines(&unit, &grid);

    assert_eq!(lines.len(), 1, "默认折叠时只显示 header");
    assert_eq!(
        lines[0].spans[1].content.as_ref(),
        sym().collapsed,
        "折叠 header 左侧应显示展开按钮"
    );
    assert!(!all_text(&lines).contains("sensitive reminder body"));
    let sem = THEME_ATOM.state().read().semantic;
    for span in &lines[0].spans {
        if !span.content.trim().is_empty() && span.content.as_ref() != "\u{2502}" {
            assert_eq!(span.style.fg, Some(sem.text.dim));
            assert!(
                !span.style.add_modifier.contains(Modifier::BOLD),
                "header 不应使用 bold"
            );
        }
    }
}

#[test]
fn test_compact_continuation_hint_renders_as_simple_system_prompt() {
    crate::i18n::init(Some("en"));
    let text = format!(
        "{}\n\ncompact summary body",
        peri_acp_types::compact::CONTINUATION_HINT
    );
    let unit = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text));
    let lines = vm_to_lines(&unit, &GridSpec::with_content(80));
    let rendered = all_text(&lines);

    assert_eq!(lines.len(), 1, "compact 控制消息应简洁显示为单行");
    assert!(rendered.contains("System Prompt"));
    assert!(!rendered.contains("[Context has been compacted"));
    assert!(!rendered.contains("compact summary body"));
}

#[test]
fn test_system_reminder_expanded_shows_body() {
    let mut reminder = TuiSystemReminder::legacy("first line\nsecond line".into());
    reminder.fold = FoldState::Expanded;
    reminder.recompute_hash();
    let unit = TuiRenderUnit::TuiSystemReminder(reminder);
    let grid = GridSpec::with_content(80);
    let lines = vm_to_lines(&unit, &grid);

    assert_eq!(lines.len(), 3, "展开后显示 header 与完整正文");
    assert_eq!(
        lines[0].spans[1].content.as_ref(),
        sym().expanded,
        "展开 header 左侧应显示收起按钮"
    );
    let text = all_text(&lines);
    assert!(text.contains("first line"));
    assert!(text.contains("second line"));
}

// ── T4：用户气泡 @image 行（image-p0-p1-spec §4）────────────────────────

/// 最小合法 PNG（签名 + IHDR + IEND，CRC 正确）——T5 校验仅需 header，
/// 无需真实像素数据。
const TINY_PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\x00IEND\xaeB\x60\x82";

/// @image 行 → meta 行（文件名 · 大小）；绝对路径不直接出现在默认渲染行
/// （§6.2-5 路径泄漏约束）。
#[test]
fn test_user_image_line_renders_meta() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    std::fs::write(&file, TINY_PNG).unwrap();
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("@image {}", file.display())));
    let lines = vm_to_lines(&vm, &grid);
    let text = all_text(&lines);
    assert!(
        text.contains("[Image: a.png · "),
        "meta 前缀（文件名 · 大小），实际: {text:?}"
    );
    assert!(text.contains(" B"), "大小跟随 meta（B 档），实际: {text:?}");
    let abs = file.canonicalize().unwrap();
    assert!(
        !text.contains(abs.to_str().unwrap()),
        "默认渲染行不暴露绝对路径，实际: {text:?}"
    );
}

/// 大小格式三档（B/KB/MB，§4.4 human_size）。
#[test]
fn test_user_image_size_formats() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let tiny = dir.path().join("tiny.png");
    std::fs::write(&tiny, TINY_PNG).unwrap();
    let mid = dir.path().join("mid.png");
    std::fs::write(&mid, vec![b'x'; 2048]).unwrap(); // 2.0 KB（显示层不校验格式）
    let big = dir.path().join("big.png");
    std::fs::write(&big, vec![b'y'; 3 * 1024 * 1024]).unwrap(); // 3.0 MB

    let grid = GridSpec::grid_for(120);
    let text_of = |name: &str| {
        let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!(
            "@image {}",
            dir.path().join(name).display()
        )));
        all_text(&vm_to_lines(&vm, &grid))
    };
    assert!(
        text_of("tiny.png").contains(" · 45 B]"),
        "B 档（TINY_PNG 45 字节），实际: {:?}",
        text_of("tiny.png")
    );
    assert!(
        text_of("mid.png").contains(" · 2.0 KB]"),
        "KB 档，实际: {:?}",
        text_of("mid.png")
    );
    assert!(
        text_of("big.png").contains(" · 3.0 MB]"),
        "MB 档，实际: {:?}",
        text_of("big.png")
    );
}

/// 文件不存在 → missing 文案（i18n key）。
#[test]
fn test_user_image_missing() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("@image /no/such/file.png".into()));
    let lines = vm_to_lines(&vm, &grid);
    let text = all_text(&lines);
    assert!(
        text.contains("file.png · missing"),
        "缺失文案，实际: {text:?}"
    );
}

/// 非图片行（@image 无路径 / @imagefoo / @image 中间空格）→ 走原
/// emphasize_user_line 路径（原样渲染，不产生 meta 行）。
#[test]
fn test_user_image_line_negative_cases() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    for text in ["@image", "@imagefoo", "@image ", " @image "] {
        let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text.into()));
        let lines = vm_to_lines(&vm, &grid);
        // 剥离网格前缀列（outer 空 / │ / gap）后应与输入完全一致——
        // 走原 emphasize_user_line 路径，无 meta 替换。
        let body = strip_visual_prefix(&lines[1], &line_text(&lines[1]), &grid, 1);
        assert_eq!(
            body, text,
            "非图片行原样渲染，实际 {body:?}（输入 {text:?}）"
        );
        assert!(
            !all_text(&lines).contains("Image:"),
            "不产生 meta 行，实际: {:?}（输入 {text:?}）",
            all_text(&lines)
        );
    }
}

/// 受管理与手工路径渲染文本一致（显示层不暴露绝对路径、无差异化文案；
/// §6.1 Q6——差异仅在 T7 预览资格）。
#[test]
fn test_user_image_managed_and_manual_same_rendering() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let managed_root = dir.path().join(".peri").join("images");
    std::fs::create_dir_all(&managed_root).unwrap();
    let managed_file = managed_root.join("m.png");
    std::fs::write(&managed_file, TINY_PNG).unwrap();
    let manual_dir = dir.path().join("elsewhere");
    std::fs::create_dir_all(&manual_dir).unwrap();
    let manual_file = manual_dir.join("m.png"); // 同名文件
    std::fs::write(&manual_file, TINY_PNG).unwrap();

    let grid = GridSpec::grid_for(120);
    let render = |path: &std::path::Path| {
        let vm =
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("@image {}", path.display())));
        all_text(&vm_to_lines(&vm, &grid))
    };
    assert_eq!(
        render(&managed_file),
        render(&manual_file),
        "受管理与手工路径显示层一致"
    );
}

/// build_image_meta_info 分级（managed_root 注入版）：Managed/Manual/Other
/// 判定 + missing 文案 + meta 文本组装。
#[test]
fn test_build_image_meta_info_grading() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".peri").join("images");
    std::fs::create_dir_all(&root).unwrap();
    let managed = root.join("a.png");
    std::fs::write(&managed, TINY_PNG).unwrap();
    let (display, size, is_managed, meta) =
        build_image_meta_info(managed.to_str().unwrap(), Some(&root));
    assert!(is_managed, "受管理目录内 → Managed");
    assert!(meta.contains("a.png"), "meta 含文件名，实际 {meta:?}");
    assert!(size.contains("B"), "大小文案，实际 {size:?}");
    assert!(meta.contains(&size), "meta 含大小文案，实际 {meta:?}");
    assert!(
        !meta.contains(display.trim_start_matches('/')),
        "meta 不暴露路径（仅文件名），实际 {meta:?}"
    );

    let manual = dir.path().join("b.png");
    std::fs::write(&manual, TINY_PNG).unwrap();
    let (_, _, is_managed2, _) = build_image_meta_info(manual.to_str().unwrap(), Some(&root));
    assert!(!is_managed2, "目录外 → Manual");

    let gone = dir.path().join("gone.png");
    let (_, size3, is_managed3, meta3) = build_image_meta_info(gone.to_str().unwrap(), Some(&root));
    assert!(!is_managed3, "不存在 → Other（非 Managed）");
    assert_eq!(size3, "missing");
    assert!(meta3.contains("missing"));
}

/// vm_to_lines_cached 收集 image_lines（logical_idx / path / managed / size_text）。
#[test]
fn test_user_image_line_info_collected() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    std::fs::write(&file, TINY_PNG).unwrap();
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!(
        "@image {}\nhello",
        file.display()
    )));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    assert_eq!(image_lines.len(), 1, "仅 @image 行产生映射信息");
    let info = &image_lines[0];
    assert_eq!(info.logical_idx, 1, "首行是 turn 节拍空行，meta 行在索引 1");
    assert_eq!(
        info.path,
        file.canonicalize().unwrap().to_str().unwrap(),
        "展示路径为 canonicalize 后路径"
    );
    assert!(!info.managed);
    assert!(info.size_text.contains("B"));
    assert!(all_text(&lines).contains("[Image: a.png ·"));
}

/// hover 行渲染：绝对路径 + accent 高亮（sem.accents.user）+ 单行截断。
#[test]
fn test_render_image_hover_line_absolute_path_highlight() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let hover = crate::kit::message_area::ImageHoverState {
        row: 5,
        slot_index: 0,
        logical_idx: 1,
        vm_hash: 7,
        path: "/long/path/to/very/deep/directory/a.png".into(),
        size_text: "45 B".into(),
    };
    let line = super::render_image_hover_line(&hover, &grid, &sem);
    let text = line_text(&line);
    assert!(
        text.contains("/long/path/to/very/deep/directory/a.png"),
        "hover 显示绝对路径，实际 {text:?}"
    );
    assert!(text.contains("45 B"), "大小文案保留，实际 {text:?}");
    let emphasized = line
        .spans
        .iter()
        .filter(|s| s.style.fg == Some(sem.accents.user))
        .count();
    assert!(emphasized > 0, "hover 行 accent 高亮（sem.accents.user）");
}

/// hover 行超宽截断：不折行（布局稳定，§4.4 单行口径）。
#[test]
fn test_render_image_hover_line_truncates() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(40); // content = 34
    let sem = THEME_ATOM.state().read().semantic;
    let hover = crate::kit::message_area::ImageHoverState {
        row: 5,
        slot_index: 0,
        logical_idx: 1,
        vm_hash: 7,
        path: "/this/is/a/very/long/absolute/path/with/many/segments/a.png".into(),
        size_text: "45 B".into(),
    };
    let line = super::render_image_hover_line(&hover, &grid, &sem);
    let text = line_text(&line);
    assert!(
        text.width() <= grid.cont_prefix_width() + grid.content_width() + 1,
        "hover 行截断到 content 宽度（含前缀列；truncate 省略号 +1），实际宽度 {}: {text:?}",
        text.width()
    );
    assert!(!text.ends_with("a.png"), "超宽时尾部被截断，实际 {text:?}");
}
