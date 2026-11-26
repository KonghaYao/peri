use super::FoldState;
use super::TuiRenderUnit;
use super::TuiSystemReminder;
use super::TuiUserBubble;

use crate::kit::entry_render_cache::EntryRenderOptions;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render;
use crate::kit::message_area::render::helpers;
use crate::kit::message_area::render::semantic;
use crate::kit::message_area::render::user;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

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
    let lines = render::vm_to_lines(&vm, &grid);

    // 6 正文 + 1 `… +6 lines`（Wide 无节拍空行，台阶承担块分隔；无 role label 行）
    assert_eq!(lines.len(), 7, "6 行上限 + 省略行，其余截断");
    let last = super::line_text(&lines[lines.len() - 1]);
    assert!(
        last.contains("+6 lines"),
        "省略行格式 `… +N lines`，实际 {last:?}"
    );
    assert!(
        !super::all_text(&lines).contains("line 9"),
        "第 7 行起应被截断"
    );

    // 无气泡背景：所有 span bg 均为 None
    for line in &lines {
        for span in &line.spans {
            assert!(span.style.bg.is_none(), "用户消息不应有全宽背景");
        }
    }
    // 不再使用 ❯（§6.1 移除）
    assert!(!super::all_text(&lines).contains('\u{276f}'));
}

/// slash/@ 局部强调（§6.1）：token 用 accent.user。
#[test]
fn test_user_slash_at_emphasis() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        "run /build and ping @matt".into(),
    ));
    let lines = render::vm_to_lines(&vm, &grid);
    let body = &lines[0];
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
    let lines = render::vm_to_lines(&unit, &grid);

    assert_eq!(lines.len(), 1, "默认折叠时只显示 header");
    assert_eq!(
        lines[0].spans[1].content.as_ref(),
        helpers::sym().collapsed,
        "折叠 header 左侧应显示展开按钮"
    );
    assert!(!super::all_text(&lines).contains("sensitive reminder body"));
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
    let lines = render::vm_to_lines(&unit, &GridSpec::with_content(80));
    let rendered = super::all_text(&lines);

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
    let lines = render::vm_to_lines(&unit, &grid);

    assert_eq!(lines.len(), 3, "展开后显示 header 与完整正文");
    assert_eq!(
        lines[0].spans[1].content.as_ref(),
        helpers::sym().expanded,
        "展开 header 左侧应显示收起按钮"
    );
    let text = super::all_text(&lines);
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
    let dir = tempfile::tempdir()
        .expect("user image line renders meta: isolated fixture workspace must remain accessible");
    let file = dir.path().join("a.png");
    std::fs::write(&file, TINY_PNG)
        .expect("user image line renders meta: isolated fixture workspace must remain accessible");
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("@image {}", file.display())));
    let lines = render::vm_to_lines(&vm, &grid);
    let text = super::all_text(&lines);
    assert!(
        text.contains("[Image: a.png · "),
        "meta 前缀（文件名 · 大小），实际: {text:?}"
    );
    assert!(text.contains(" B"), "大小跟随 meta（B 档），实际: {text:?}");
    let abs = file
        .canonicalize()
        .expect("user image line renders meta: isolated fixture workspace must remain accessible");
    assert!(
        !text.contains(abs.to_str().expect(
            "user image line renders meta: fixture paths and protocol text must be valid UTF-8"
        )),
        "默认渲染行不暴露绝对路径，实际: {text:?}"
    );
}

/// 大小格式三档（B/KB/MB，§4.4 human_size）。
#[test]
fn test_user_image_size_formats() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir()
        .expect("user image size formats: isolated fixture workspace must remain accessible");
    let tiny = dir.path().join("tiny.png");
    std::fs::write(&tiny, TINY_PNG)
        .expect("user image size formats: isolated fixture workspace must remain accessible");
    let mid = dir.path().join("mid.png");
    std::fs::write(&mid, vec![b'x'; 2048])
        .expect("user image size formats: isolated fixture workspace must remain accessible"); // 2.0 KB（显示层不校验格式）
    let big = dir.path().join("big.png");
    std::fs::write(&big, vec![b'y'; 3 * 1024 * 1024])
        .expect("user image size formats: isolated fixture workspace must remain accessible"); // 3.0 MB

    let grid = GridSpec::grid_for(120);
    let text_of = |name: &str| {
        let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!(
            "@image {}",
            dir.path().join(name).display()
        )));
        super::all_text(&render::vm_to_lines(&vm, &grid))
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
    let lines = render::vm_to_lines(&vm, &grid);
    let text = super::all_text(&lines);
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
        let lines = render::vm_to_lines(&vm, &grid);
        // 剥离网格前缀列（outer 空 / │ / gap）后应与输入完全一致——
        // 走原 emphasize_user_line 路径，无 meta 替换。
        let body = semantic::strip_visual_prefix(&lines[0], &super::line_text(&lines[0]), &grid, 0);
        assert_eq!(
            body, text,
            "非图片行原样渲染，实际 {body:?}（输入 {text:?}）"
        );
        assert!(
            !super::all_text(&lines).contains("Image:"),
            "不产生 meta 行，实际: {:?}（输入 {text:?}）",
            super::all_text(&lines)
        );
    }
}

/// 受管理与手工路径渲染文本一致（显示层不暴露绝对路径、无差异化文案；
/// §6.1 Q6——差异仅在 T7 预览资格）。
#[test]
fn test_user_image_managed_and_manual_same_rendering() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect("user image managed and manual same rendering: isolated fixture workspace must remain accessible");
    let managed_root = dir.path().join(".peri").join("images");
    std::fs::create_dir_all(&managed_root).expect("user image managed and manual same rendering: isolated fixture workspace must remain accessible");
    let managed_file = managed_root.join("m.png");
    std::fs::write(&managed_file, TINY_PNG).expect("user image managed and manual same rendering: isolated fixture workspace must remain accessible");
    let manual_dir = dir.path().join("elsewhere");
    std::fs::create_dir_all(&manual_dir).expect("user image managed and manual same rendering: isolated fixture workspace must remain accessible");
    let manual_file = manual_dir.join("m.png"); // 同名文件
    std::fs::write(&manual_file, TINY_PNG).expect("user image managed and manual same rendering: isolated fixture workspace must remain accessible");

    let grid = GridSpec::grid_for(120);
    let render = |path: &std::path::Path| {
        let vm =
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("@image {}", path.display())));
        super::all_text(&render::vm_to_lines(&vm, &grid))
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
    let dir = tempfile::tempdir()
        .expect("build image meta info grading: isolated fixture workspace must remain accessible");
    let root = dir.path().join(".peri").join("images");
    std::fs::create_dir_all(&root)
        .expect("build image meta info grading: isolated fixture workspace must remain accessible");
    let managed = root.join("a.png");
    std::fs::write(&managed, TINY_PNG)
        .expect("build image meta info grading: isolated fixture workspace must remain accessible");
    let (display, size, is_managed, meta) = user::build_image_meta_info(
        managed.to_str().expect(
            "build image meta info grading: fixture paths and protocol text must be valid UTF-8",
        ),
        Some(&root),
    );
    assert!(is_managed, "受管理目录内 → Managed");
    assert!(meta.contains("a.png"), "meta 含文件名，实际 {meta:?}");
    assert!(size.contains("B"), "大小文案，实际 {size:?}");
    assert!(meta.contains(&size), "meta 含大小文案，实际 {meta:?}");
    assert!(
        !meta.contains(display.trim_start_matches('/')),
        "meta 不暴露路径（仅文件名），实际 {meta:?}"
    );

    let manual = dir.path().join("b.png");
    std::fs::write(&manual, TINY_PNG)
        .expect("build image meta info grading: isolated fixture workspace must remain accessible");
    let (_, _, is_managed2, _) = user::build_image_meta_info(
        manual.to_str().expect(
            "build image meta info grading: fixture paths and protocol text must be valid UTF-8",
        ),
        Some(&root),
    );
    assert!(!is_managed2, "目录外 → Manual");

    let gone = dir.path().join("gone.png");
    let (_, size3, is_managed3, meta3) = user::build_image_meta_info(
        gone.to_str().expect(
            "build image meta info grading: fixture paths and protocol text must be valid UTF-8",
        ),
        Some(&root),
    );
    assert!(!is_managed3, "不存在 → Other（非 Managed）");
    assert_eq!(size3, "missing");
    assert!(meta3.contains("missing"));
}

/// vm_to_lines_cached 收集 image_lines（logical_idx / path / managed / size_text）。
#[test]
fn test_user_image_line_info_collected() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect(
        "user image line info collected: isolated fixture workspace must remain accessible",
    );
    let file = dir.path().join("a.png");
    std::fs::write(&file, TINY_PNG).expect(
        "user image line info collected: isolated fixture workspace must remain accessible",
    );
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!(
        "@image {}\nhello",
        file.display()
    )));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    assert_eq!(image_lines.len(), 1, "仅 @image 行产生映射信息");
    let info = &image_lines[0];
    assert_eq!(info.logical_idx, 0, "Wide 无前导空行，meta 行在索引 0");
    assert_eq!(
        info.path,
        file.canonicalize().expect("user image line info collected: the canonicalize fixture operation uses valid inputs").to_str().expect("user image line info collected: fixture paths and protocol text must be valid UTF-8"),
        "展示路径为 canonicalize 后路径"
    );
    assert!(!info.managed);
    assert!(info.size_text.contains("B"));
    assert!(super::all_text(&lines).contains("[Image: a.png ·"));
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
    let line = render::render_image_hover_line(&hover, &grid, &sem);
    let text = super::line_text(&line);
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
    let line = render::render_image_hover_line(&hover, &grid, &sem);
    let text = super::line_text(&line);
    assert!(
        text.width() <= grid.cont_prefix_width() + grid.content_width() + 1,
        "hover 行截断到 content 宽度（含前缀列；truncate 省略号 +1），实际宽度 {}: {text:?}",
        text.width()
    );
    assert!(!text.ends_with("a.png"), "超宽时尾部被截断，实际 {text:?}");
}

// ── 上传式附件（粘贴图片）：消息区附件记录 ──────────────────────────────

use crate::kit::tui_render_unit::UserAttachment;

/// 文本 + 附件：正文之外多一行附件记录（图片标识 + 名称 + 大小）。
#[test]
fn test_user_bubble_renders_attachment_record_with_text() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        "look at this".into(),
        vec![UserAttachment::for_bytes(45)],
    ));
    let lines = render::vm_to_lines(&vm, &grid);
    let text = super::all_text(&lines);
    assert!(
        text.contains("[Image] Image 1 · 45 B"),
        "附件记录行（标识 · 名称 · 大小），实际: {text:?}"
    );
    assert!(text.contains("look at this"), "正文保留，实际: {text:?}");
}

/// 图片-only 提交（空文本 + 附件）：必须留下附件记录（此前渲染 0 行）。
#[test]
fn test_user_bubble_image_only_renders_attachment_record() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        String::new(),
        vec![UserAttachment::for_bytes(2048)],
    ));
    let lines = render::vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "Wide 仅附件记录行（无节拍空行）");
    let text = super::all_text(&lines);
    assert!(text.contains("[Image] Image 1 · 2.0 KB"), "实际: {text:?}");
}

/// 多张附件逐张编号，可区分（名称与提交顺序一致）。
#[test]
fn test_user_bubble_numbers_multiple_attachments() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        "two shots".into(),
        vec![
            UserAttachment::for_bytes(45),
            UserAttachment::for_bytes(2048),
        ],
    ));
    let text = super::all_text(&render::vm_to_lines(&vm, &grid));
    assert!(
        text.contains("[Image] Image 1 · 45 B"),
        "第一张，实际: {text:?}"
    );
    assert!(
        text.contains("[Image] Image 2 · 2.0 KB"),
        "第二张，实际: {text:?}"
    );
}

/// 空文本 + 无附件（rewind/thinking 回传等）仍渲染 0 行——不产生悬空 turn 节拍。
#[test]
fn test_user_bubble_empty_text_without_attachments_renders_nothing() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(String::new()));
    assert!(render::vm_to_lines(&vm, &grid).is_empty());
}

/// 非图片的 @文件引用不受影响：正文原样渲染，且不产生附件记录行。
#[test]
fn test_user_bubble_file_mention_unaffected_by_attachment_record() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("read @src/main.rs please".into()));
    let lines = render::vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "Wide 仅正文行（无节拍空行）");
    let text = super::all_text(&lines);
    assert!(
        text.contains("@src/main.rs"),
        "文件引用原样，实际: {text:?}"
    );
    assert!(!text.contains("[Image]"), "非图片引用不产生附件记录");
}

// ── 用户气泡整行 markdown 图片（终端像素协议）───────────────────────────

fn write_red_png(path: &std::path::Path) {
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([220, 30, 30, 255]));
    image
        .save_with_format(path, image::ImageFormat::Png)
        .expect("user inline image: png fixture must encode");
}

/// 整行仅 `![alt](path)` 且缓存命中 → 真实行数的空白占位 + 内联放置；
/// 计入 6 行用户正文预算，不再输出原始语法或半块字符。
#[test]
fn test_user_bubble_standalone_image_renders_placement_when_ready() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect("user inline image: tempdir must exist");
    let file = dir.path().join("red.png");
    write_red_png(&file);
    // markdown 链接里反斜杠是转义符（`\.` 会丢反斜杠），Windows 路径统一用正斜杠。
    let source = file.to_str().expect("utf8 path").replace('\\', "/");
    let source = source.as_str();
    let grid = GridSpec::grid_for(120);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(
        crate::kit::image_view::request_until_ready(
            crate::kit::image_view::ImageSource::File(source),
            crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS),
            std::time::Duration::from_secs(5)
        ),
        "图片缓存应在渲染前就绪"
    );

    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("![a]({source})")));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = render::vm_to_lines_cached(&vm, &grid, &mut cache, false);
    let text = super::all_text(&lines);
    assert_eq!(image_lines.len(), 1, "整行图片应产出一处内联放置");
    let info = &image_lines[0];
    let placement = info.inline.as_ref().expect("应携带协议句柄");
    assert!(placement.rows >= 1);
    // 等比铺满 + 在正文宽内水平居中。
    let body_cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    let prefix = u16::try_from(grid.cont_prefix_width()).expect("prefix fits u16");
    assert_eq!(
        placement.x_offset,
        prefix + (body_cols - placement.cols) / 2,
        "用户气泡图片在正文列内居中"
    );
    assert!(
        placement.cols == body_cols || placement.rows == crate::kit::inline_image::USER_IMAGE_ROWS,
        "受限轴必须贴边（铺满）：cols={} rows={}",
        placement.cols,
        placement.rows
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.content.contains('█'))),
        "像素路径不得输出半块字符，实际 {text:?}"
    );
    assert!(!text.contains("![a]"), "命中后不再显示原始图片语法");
    assert!(!text.contains("[Image:"), "不产生 @image meta 行");
    assert!(
        lines.len() <= usize::from(placement.rows) + 1,
        "图片行使用独立预算（不占正文 6 行），最多再加 1 行尾随空行，实际 {} 行",
        lines.len()
    );
}

/// 上传附件（粘贴图片）在转录区按摘要查上传仓库：Ready → 预留 ready.rows 行，
/// meta 记录行照旧；图片行使用独立预算，不占正文 6 行。
#[test]
#[serial_test::serial]
fn test_user_bubble_uploaded_image_renders_placement_when_ready() {
    crate::i18n::init(Some("en"));
    crate::kit::image_view::release_all_uploads();
    let dir = tempfile::tempdir().expect("uploaded image: tempdir must exist");
    let file = dir.path().join("red.png");
    write_red_png(&file);
    let payload = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(&file).expect("read png fixture"))
    };
    let attachment =
        crate::kit::tui_render_unit::UserAttachment::from_base64_payload(&payload, "image/png");
    let id = attachment.upload_id();
    crate::kit::image_view::retain_upload(id, std::sync::Arc::from(payload.as_str()));

    let grid = GridSpec::grid_for(120);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(
        crate::kit::image_view::request_until_ready(
            crate::kit::image_view::ImageSource::Uploaded(id),
            crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS,),
            std::time::Duration::from_secs(5),
        ),
        "上传图片应能编码就绪"
    );
    let ready_rows = crate::kit::image_view::lookup(
        &crate::kit::image_view::ImageSource::Uploaded(id),
        crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS),
    )
    .expect("Ready")
    .rows;

    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        "see attached".to_string(),
        vec![attachment],
    ));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = render::vm_to_lines_cached(&vm, &grid, &mut cache, false);
    assert_eq!(image_lines.len(), 1, "上传附件应产出一处内联放置");
    let info = &image_lines[0];
    let placement = info.inline.as_ref().expect("inline placement");
    assert_eq!(placement.rows, ready_rows, "占位行数 == 实际编码行数");
    let text = super::all_text(&lines);
    assert!(
        text.contains("[Image] Image 1"),
        "meta 记录行保留：{text:?}"
    );
    assert!(
        (info.logical_idx..info.logical_idx + usize::from(placement.rows)).all(|idx| {
            let text: String = lines[idx]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            !text.contains('█') && !text.chars().any(char::is_alphanumeric)
        }),
        "图片占位行只有网格前缀"
    );
    crate::kit::image_view::release_all_uploads();
}

/// 上传附件未就绪（仓库无载荷）时只保留 meta 行，不产生空白预留。
#[test]
#[serial_test::serial]
fn test_user_bubble_uploaded_image_without_payload_shows_meta_only() {
    crate::i18n::init(Some("en"));
    crate::kit::image_view::release_all_uploads();
    let attachment =
        crate::kit::tui_render_unit::UserAttachment::from_base64_payload("AQID", "image/png");
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        String::new(),
        vec![attachment],
    ));
    let grid = GridSpec::grid_for(120);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = render::vm_to_lines_cached(&vm, &grid, &mut cache, false);
    assert!(image_lines.is_empty(), "未命中不得预留像素区");
    let text = super::all_text(&lines);
    assert!(text.contains("[Image] Image 1"), "只保留 meta 行：{text:?}");
}

/// 多图总预算 10 行：6 行大图之后剩余 4 行放不下第二张 6 行图，
/// 后续附件只显示 meta 行（不产生越界占位）。
#[test]
#[serial_test::serial]
fn test_user_bubble_image_total_budget_caps_multi_image() {
    crate::i18n::init(Some("en"));
    crate::kit::image_view::release_all_uploads();
    let dir = tempfile::tempdir().expect("multi image budget: tempdir must exist");
    let file = dir.path().join("red.png");
    write_red_png(&file);
    let payload = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(&file).expect("read png fixture"))
    };
    // 三张同一图片（同摘要）：第一张占 6 行（上限），剩余 4 行放不下第二张。
    let attachment =
        crate::kit::tui_render_unit::UserAttachment::from_base64_payload(&payload, "image/png");
    let id = attachment.upload_id();
    crate::kit::image_view::retain_upload(id, std::sync::Arc::from(payload.as_str()));
    let grid = GridSpec::grid_for(120);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(crate::kit::image_view::request_until_ready(
        crate::kit::image_view::ImageSource::Uploaded(id),
        crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS),
        std::time::Duration::from_secs(5),
    ));
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        String::new(),
        vec![attachment, attachment, attachment],
    ));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines) = render::vm_to_lines_cached(&vm, &grid, &mut cache, false);
    let rows: usize = image_lines
        .iter()
        .map(|info| usize::from(info.inline.as_ref().map_or(0, |p| p.rows)))
        .sum();
    assert!(
        rows <= usize::from(crate::kit::inline_image::USER_IMAGE_TOTAL_ROWS),
        "多图总行数不得超过 10 行，实际 {rows}"
    );
    assert_eq!(image_lines.len(), 1, "第二张起只显示 meta 行");
    let text = super::all_text(&lines);
    assert_eq!(
        text.matches("[Image] Image").count(),
        3,
        "三张附件都保留 meta 记录：{text:?}"
    );
    crate::kit::image_view::release_all_uploads();
}

/// 无 draw hook 的 surface（scheduled/detail：inline_pixels=false）不得把已就绪
/// 图片替换成空白占位——否则 detail 留下无法填充的空行。
#[test]
fn test_user_bubble_standalone_image_kept_as_text_without_inline_surface() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect("user inline image: tempdir must exist");
    let file = dir.path().join("red.png");
    write_red_png(&file);
    // markdown 链接里反斜杠是转义符（`\.` 会丢反斜杠），Windows 路径统一用正斜杠。
    let source = file.to_str().expect("utf8 path").replace('\\', "/");
    let source = source.as_str();
    let grid = GridSpec::grid_for(120);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(
        crate::kit::image_view::request_until_ready(
            crate::kit::image_view::ImageSource::File(source),
            crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS),
            std::time::Duration::from_secs(5)
        ),
        "图片缓存应在渲染前就绪"
    );

    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("![a]({source})")));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, image_lines, _, _, _) = render::vm_to_lines_with_options(
        &vm,
        &grid,
        &mut cache,
        None,
        EntryRenderOptions::isolated(crate::kit::message_area::grid::ZoneTransition::None, false),
    );
    let text = super::all_text(&lines);
    assert!(
        image_lines.is_empty(),
        "无 draw hook 的 surface 不产出内联放置"
    );
    assert!(text.contains("![a]"), "保留原始文本，实际 {text:?}");
}

/// 行内混排图片不参与替换（MVP 只识别整行语法）。
#[test]
fn test_user_bubble_inline_image_syntax_kept_as_text() {
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect("user inline image: tempdir must exist");
    let file = dir.path().join("red.png");
    write_red_png(&file);
    // markdown 链接里反斜杠是转义符（`\.` 会丢反斜杠），Windows 路径统一用正斜杠。
    let source = file.to_str().expect("utf8 path").replace('\\', "/");
    let source = source.as_str();
    let grid = GridSpec::grid_for(120);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(
        crate::kit::image_view::request_until_ready(
            crate::kit::image_view::ImageSource::File(source),
            crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::USER_IMAGE_ROWS),
            std::time::Duration::from_secs(5)
        ),
        "图片缓存应在渲染前就绪"
    );

    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("see ![a]({source})")));
    let lines = render::vm_to_lines(&vm, &grid);
    let text = super::all_text(&lines);
    assert!(
        !lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.content.contains('█'))),
        "行内混排不替换为图片，实际 {text:?}"
    );
    assert!(text.contains("![a]"), "保留原始文本，实际 {text:?}");
}
