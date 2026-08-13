//! 基础设施：前置扫描（pulldown-cmark `into_offset_iter`）+ 占位 token 替换 + 结构投影。
//!
//! 实验目标（.peri/plans/image-rendering-research.md §3.3 / §8.3 spike 清单第 4 项）：
//! 验证「前置扫描 + 占位 token 替换」是否破坏 ratatui-kit-markdown 0.3.0 的块结构。

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::text::Span;
use ratatui_kit_markdown::{ParsedBlock, parse_markdown};

// ── 1. 前置扫描 ─────────────────────────────────────────────────

/// 单个图片命中：字节区间基于**原始 markdown 文本**，side table 可映射回原区间。
#[derive(Debug, Clone)]
pub struct ImageHit {
    pub byte_start: usize,
    pub byte_end: usize,
    /// alt 文本：pulldown-cmark 0.12 的 `Tag::Image` **没有** alt 字段，
    /// 需要从 Start/End 之间的 `Event::Text` 拼接（与 0.13 的 alt 字段同语义：
    /// 去除标记符号后的纯文本）。
    pub alt: String,
    pub url: String,
    pub title: String,
    /// reference 形式 `![a][ref]` 的引用 id；inline 形式为空串。
    pub id: String,
}

/// 与 ratatui-kit-markdown 0.3.0 `parse_markdown`（parser.rs:448）完全一致的 Options。
/// 前置扫描与 rk_parse 的 Options 必须一致，否则同一段文本会被两种解析器
/// 判定为不同结构（§3.5 风险）。
pub fn md_options() -> Options {
    Options::all() - Options::ENABLE_SMART_PUNCTUATION
}

/// 前置扫描：枚举 `Tag::Image`，收集字节区间 + url/title/id + 拼接 alt。
///
/// 事件流顺序（OffsetIter 先序遍历）：`Start(Tag::Image)` → inner 事件
/// （alt 的 `Text`/`Code`）→ `End(TagEnd::Image)`；用栈记录当前活跃图片，
/// 嵌套图片（alt 内再嵌图片）也能正确归属。
pub fn scan_images(src: &str) -> Vec<ImageHit> {
    let mut hits: Vec<ImageHit> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    for (event, range) in Parser::new_ext(src, md_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image {
                dest_url, title, id, ..
            }) => {
                let idx = hits.len();
                hits.push(ImageHit {
                    byte_start: range.start,
                    byte_end: range.end,
                    alt: String::new(),
                    url: dest_url.into_string(),
                    title: title.into_string(),
                    id: id.into_string(),
                });
                active.push(idx);
            }
            // alt 的文本与行内代码都产生内容事件；强调/链接的标记不产生
            // 内容事件（与 pulldown 0.13 的 alt 字段同语义：纯文本，无标记）
            Event::Text(t) | Event::Code(t) => {
                if let Some(&idx) = active.last() {
                    hits[idx].alt.push_str(t.as_ref());
                }
            }
            Event::End(TagEnd::Image) => {
                active.pop();
            }
            _ => {}
        }
    }
    hits
}

// ── 2. 占位 token 替换 ──────────────────────────────────────────

/// 占位 token 三种候选形式（对比用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `\u{0}IMG{n}\u{0}`：NUL 包裹。NUL 是 ASCII 控制字符，正常用户输入
    /// 几乎不可能出现；非 markdown 结构语法。
    Nul,
    /// `\u{E000}IMG{n}\u{E000}`：Unicode 私用区（PUA）字符包裹。
    Pua,
    /// `IMG{n}`：裸 ASCII 词（无包裹），作为结构参考基线；预期与用户
    /// 文本碰撞风险极高，用于对照。
    Plain,
}

pub fn token(kind: TokenKind, idx: usize) -> String {
    match kind {
        TokenKind::Nul => format!("\u{0}IMG{idx}\u{0}"),
        TokenKind::Pua => format!("\u{E000}IMG{idx}\u{E000}"),
        TokenKind::Plain => format!("IMG{idx}"),
    }
}

/// 逆序替换：区间基于原始文本，从后往前替换避免字节位移。
pub fn replace_images(src: &str, hits: &[ImageHit], kind: TokenKind) -> String {
    let mut out = src.to_string();
    for (i, h) in hits.iter().enumerate().rev() {
        out.replace_range(h.byte_start..h.byte_end, &token(kind, i));
    }
    out
}

/// 碰撞感知编号：为每张图片选择「源文本中尚未出现」的 token 编号，
/// 替换后 token 出现次数 == 图片数，渲染侧查表无歧义。
pub fn replace_collision_free(src: &str, hits: &[ImageHit], kind: TokenKind) -> (String, Vec<String>) {
    let mut used: Vec<usize> = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut out = src.to_string();
    for h in hits.iter().rev() {
        let mut n = 0;
        loop {
            if !used.contains(&n) && !src.contains(&token(kind, n)) {
                used.push(n);
                break;
            }
            n += 1;
        }
        let t = token(kind, n);
        tokens.push(t.clone());
        out.replace_range(h.byte_start..h.byte_end, &t);
    }
    tokens.reverse();
    (out, tokens)
}

/// 统计 token 在文本中的出现次数。
pub fn count_occurrences(text: &str, t: &str) -> usize {
    text.matches(t).count()
}

// ── 3. 结构投影 ──────────────────────────────────────────────────

/// 单个 span 的投影：`内容[样式修饰符]`。样式继承关系（token 在强调内应
/// 带 ITALIC）由此可见。
pub fn span_sig(s: &Span<'_>) -> String {
    format!("{}[{:?}]", s.content, s.style.add_modifier)
}

/// 把 ParsedBlock 投影为 (块种类, 行列表)；每行是 span 投影的列表。
/// Table 的 cell 内部用「‖」连接（cell 本身是 span 列表）。
pub fn block_rows(b: &ParsedBlock) -> (String, Vec<Vec<String>>) {
    match b {
        ParsedBlock::Heading(lvl, line) => (
            format!("Heading({lvl:?})"),
            vec![line.spans.iter().map(span_sig).collect()],
        ),
        ParsedBlock::Paragraph(lines) => (
            "Paragraph".into(),
            lines.iter().map(|l| l.spans.iter().map(span_sig).collect()).collect(),
        ),
        ParsedBlock::CodeBlock(lang, lines) => (
            format!("CodeBlock(lang={lang:?})"),
            lines.iter().map(|l| vec![format!("{l:?}")]).collect(),
        ),
        ParsedBlock::ListItem(d) => (
            format!("ListItem(depth={},ordered={})", d.depth, d.ordered),
            vec![d.spans.iter().map(span_sig).collect()],
        ),
        ParsedBlock::Table(head, body, aligns) => {
            let mut rows: Vec<Vec<String>> = Vec::new();
            let a: Vec<String> = aligns.iter().map(|x| format!("{x:?}")).collect();
            rows.push(
                head.iter()
                    .map(|cell| cell.iter().map(span_sig).collect::<Vec<_>>().join("‖"))
                    .collect(),
            );
            for r in body {
                rows.push(
                    r.iter()
                        .map(|cell| cell.iter().map(span_sig).collect::<Vec<_>>().join("‖"))
                        .collect(),
                );
            }
            (format!("Table(aligns={a:?})"), rows)
        }
        ParsedBlock::Rule => ("Rule".into(), vec![]),
    }
}

/// 结构形状：块种类 + 每行 span 数。**忽略文本内容**，用于断言
/// 「替换不破坏块结构」。
pub fn shape(b: &ParsedBlock) -> (String, Vec<usize>) {
    let (k, rows) = block_rows(b);
    (k, rows.iter().map(|r| r.len()).collect())
}

/// 解析 markdown 并返回 blocks。
pub fn parse(md: &str) -> Vec<ParsedBlock> {
    parse_markdown(md).blocks
}
