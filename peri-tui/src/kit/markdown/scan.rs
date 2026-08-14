//! 图片前置扫描器（P0 文本语义层第一步）。
//!
//! `ratatui_kit_markdown` 0.3.0 的 parser 没有 `Tag::Image` 分支（parser.rs:416
//! 落入 `_ => {}`），图片 URL/title 不可逆丢失。本模块在 sanitize 之后、
//! `rk_parse` 之前，用同版本 pulldown-cmark（`pulldown-cmark-012`，与
//! ratatui-kit-markdown 0.3.0 锁定一致）扫描 `![alt](url)`，收集字节区间与
//! 元数据，替换为占位 token 后再交给 `rk_parse`，保留周边块结构。
//!
//! 方案依据：`.peri/plans/image-rendering-research.md` §3.3（可行路径 B）、
//! `.peri/plans/spike-scan-matrix.md`（S3，45 矩阵 case 全过）、
//! `.peri/plans/spike-convert-state.md`（S1 §4 管线顺序，硬性约束）。
//!
//! 管线顺序：`input → ensure_closed_code_fences → sanitized → scan_images +
//! replace_images → placeholder → rk_parse`。扫描作用于 sanitized（fence
//! 补全后）文本：字节区间与 rk_parse 输入同一坐标系（S1 §4 理由 1），且
//! 未闭合 fence 内的 `![` 由 pulldown 代码块语义天然排除（S1 §4 理由 2）。

use pulldown_cmark_012::{Event, Options, Parser, Tag, TagEnd};

/// 与 `ratatui_kit_markdown::parse_markdown` 内部（parser.rs:448）逐位一致的
/// Options：`all() - ENABLE_SMART_PUNCTUATION`。
///
/// [为什么只允许一份字面量] 前置扫描与 rk_parse 必须使用同一 Options，否则
/// 同一文本两种解析结果（块结构错位，S3 §3.5 漂移风险）。rk_parse 的
/// Options 写死在 crate 内部、无法传入，因此本函数是 peri 侧唯一字面量
/// 来源，禁止在其他位置复制（T2 spec §2.6）。
pub(crate) fn md_options() -> Options {
    Options::all() - Options::ENABLE_SMART_PUNCTUATION
}

/// 一张完整图片语法（pulldown 0.12 的 `Tag::Image` 无 alt 字段，需从
/// Start/End 之间的 `Text`/`Code` 事件拼接，S3 §4.1）。字节区间基于扫描
/// 输入文本（sanitized 坐标系，S3 §6 建议 2）。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    /// 图片语法起点（含 `!`）。
    pub byte_start: usize,
    /// 图片语法终点（不含）。`Start(Tag::Image)` 的 range 恰好覆盖
    /// `!` 到 `)` 的完整语法（S3 §4.2），可直接索引原始文本。
    pub byte_end: usize,
    /// alt 文本：Text/Code 事件拼接；强调/链接标记已剥离、转义已还原
    /// （S3 §4.1），可直接用于文本降级展示。
    pub alt: String,
    /// dest_url。reference 形式（`![a][ref]` + 定义）时 url 已从定义解析
    /// （S3 §4.7）。
    pub url: String,
    /// title；空字符串 = 无 title。pulldown 0.12 的 title 为 `CowStr` 而非
    /// `Option`（0.13 才改为 `Option`），故不用 `Option<String>`。
    pub title: String,
    /// 图片 id：空 = inline 图片。reference 形式（`![a][ref]` + 定义）时
    /// **同样替换为占位 token** 并渲染降级文案（url 已从定义解析）——与
    /// inline 行为一致，对用户信息量更大（显示 `alt (url)`）；S3 §6 建议 9
    /// 「reference 按字面显示」未采纳（评审 P1-3 决策 b，spec 记录在案）。
    /// 流式输入时定义位于文档尾部，定义出现瞬间显示形态突变属 reference
    /// 语义固有特性（markdown 渲染器通用行为）。
    pub id: String,
    /// 实际分配的占位 token（collision-free 编号，S3 §4.5/§7.2）。
    /// side table 关联键：convert 阶段以 token 文本查表（编号唯一性由
    /// `replace_images` 保证）。`scan_images` 阶段为空串。
    pub token: String,
}

/// 前置扫描：收集文本中所有完整 `![...]` 图片语法。
///
/// Options 与 rk_parse 逐位一致（`md_options`，S3 §3.5）；`into_offset_iter()`
/// 提供字节区间。未闭合语法（`!` / `![alt]` / `![alt](` / `![alt](url`）
/// 恒不产生命中（S3 §5 F 组）——完整 `Tag::Image` 才命中，替换为无操作，
/// 流式增量缓存只需处理图片闭合瞬间（S3 §6 建议 4）。
pub fn scan_images(sanitized: &str) -> Vec<ImageInfo> {
    let mut hits: Vec<ImageInfo> = Vec::new();
    // 活跃图片索引栈：alt 内强调/链接等 span 的 Text 事件也计入 alt
    // （S3 §4.1，`![**bold**](u)` → "bold"），栈顶即当前正在拼接 alt 的图片。
    let mut active: Vec<usize> = Vec::new();
    for (event, range) in Parser::new_ext(sanitized, md_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image {
                dest_url,
                title,
                id,
                ..
            }) => {
                let idx = hits.len();
                hits.push(ImageInfo {
                    byte_start: range.start,
                    byte_end: range.end,
                    alt: String::new(),
                    url: dest_url.into_string(),
                    title: title.into_string(),
                    id: id.into_string(),
                    token: String::new(),
                });
                active.push(idx);
            }
            // alt 的普通文本与行内代码都产生内容事件；强调/链接标记本身不产生
            Event::Text(t) | Event::Code(t) => {
                if let Some(&idx) = active.last() {
                    hits[idx].alt.push_str(t.as_ref());
                }
            }
            // [P2-4 修复] softbreak 折叠补空格：`![a\nb](u)` 的 alt 语义为
            // `"a b"`（markdown softbreak 渲染为空格）；不补则拼成 `"ab"`。
            Event::SoftBreak => {
                if let Some(&idx) = active.last() {
                    hits[idx].alt.push(' ');
                }
            }
            Event::End(TagEnd::Image) => {
                let _ = active.pop();
            }
            _ => {}
        }
    }
    hits
}

/// 占位 token：`\u{0}IMG{n}\u{0}`（NUL 包裹 + 序号，S3 §4.6 推荐）。
///
/// 满足三项要求（S3 §1.2）：不与用户输入碰撞（可检测、可重编号规避）、
/// 非结构语法（无 markdown 标记字符，不破坏 `|` 行首表头检测）、可映射回
/// 原始字节区间（side table 按序号查表）。NUL 优于 PUA：显示宽度为 0
/// （漏替换也不撑宽表格/行），且用户输入/输入法路径几乎不可能产生。
fn image_token(n: usize) -> String {
    format!("\u{0}IMG{n}\u{0}")
}

/// 占位替换：把每张图片语法替换为 collision-free 的占位 token（S3 §4.5
/// 重编号解法：跳过源文本已存在的编号 + 本次已分配编号）。
///
/// 编号按源文本出现顺序正序分配（先出现的图片获得更小编号，调试/展示直觉），
/// 替换本身逆序 `replace_range` 避免区间位移（S3 §7.2）。
///
/// 返回 `(占位文本, infos)`：infos 即 side table，`token` 已按输入顺序写入
/// 各元素，与替换一一对应（编号唯一性由此保证）。
pub fn replace_images(text: &str, infos: &[ImageInfo]) -> (String, Vec<ImageInfo>) {
    let mut used: Vec<usize> = Vec::new();
    let mut with_tokens: Vec<ImageInfo> = Vec::with_capacity(infos.len());
    for info in infos {
        let n = next_collision_free_number(text, &used);
        used.push(n);
        let mut updated = info.clone();
        updated.token = image_token(n);
        with_tokens.push(updated);
    }
    let mut replaced = text.to_string();
    for info in with_tokens.iter().rev() {
        replaced.replace_range(info.byte_start..info.byte_end, &info.token);
    }
    (replaced, with_tokens)
}

/// 碰撞感知编号：跳过「本次已分配」与「源文本已含该 token 形式」的编号。
/// 用户文本恰好含 `\u{0}IMG{n}\u{0}` 时图片自动重编号（S3 §4.5 G1/G2）。
fn next_collision_free_number(text: &str, used: &[usize]) -> usize {
    let mut n = 0;
    loop {
        if !used.contains(&n) && !text.contains(&image_token(n)) {
            return n;
        }
        n += 1;
    }
}

// ── 测试（S3 矩阵关键用例 + T2 spec 2.5 验收）───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言命中切片以 `![` 开头、以 `)` 结尾（S3 矩阵「切片」列口径）。
    fn assert_slices(src: &str, hits: &[ImageInfo]) {
        assert!(!hits.is_empty(), "应至少 1 个命中");
        for h in hits {
            let slice = &src[h.byte_start..h.byte_end];
            assert!(slice.starts_with("!["), "切片应以 ![ 开头: {slice:?}");
            assert!(slice.ends_with(')'), "切片应以 ) 结尾: {slice:?}");
            assert!(h.byte_start < h.byte_end, "区间应非空");
        }
    }

    // ── A 组：段落内图片 ──────────────────────────────────────────

    #[test]
    fn scan_standalone_paragraph() {
        // A1/A2：独立图片段，前后有段落
        let src = "before\n\n![alt](url)\n\nafter";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        assert_eq!(&src[hits[0].byte_start..hits[0].byte_end], "![alt](url)");
        assert_eq!(hits[0].alt, "alt");
        assert_eq!(hits[0].url, "url");
        assert_eq!(hits[0].id, "", "inline 图片 id 应为空");
    }

    #[test]
    fn scan_inline_middle_start_end() {
        // A3/A4/A5：段落内中置/行首/行尾
        let src = "before ![a](u) after";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        assert_slices(src, &hits);
        assert_eq!(hits[0].alt, "a");

        assert_eq!(scan_images("![a](u) and text").len(), 1, "A4 行首");
        assert_eq!(scan_images("text and ![a](u)").len(), 1, "A5 行尾");
    }

    #[test]
    fn scan_multiple_same_paragraph() {
        // A6/A7/A8：同段多图（空格/无空格）、多段多图
        let src = "![a](u) ![b](v)";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 2);
        assert_slices(src, &hits);
        assert_eq!(&src[hits[0].byte_start..hits[0].byte_end], "![a](u)");
        assert_eq!(&src[hits[1].byte_start..hits[1].byte_end], "![b](v)");

        assert_eq!(scan_images("![a](u)![b](v)").len(), 2, "A7 无空格");
        assert_eq!(
            scan_images("![a](u)\n\n![b](v)\n\n![c](w)").len(),
            3,
            "A8 多段多图"
        );
    }

    // ── B 组：列表项内图片 ────────────────────────────────────────

    #[test]
    fn scan_list_item() {
        // B1/B2：列表项内图片（独占 / 图文混排）
        let src = "- item ![a](u) tail";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        assert_slices(src, &hits);
        assert_eq!(hits[0].alt, "a");
        assert_eq!(scan_images("- ![a](u)").len(), 1, "B1 列表项独占图片");
    }

    // ── F 组：流式未闭合（S3 §5）──────────────────────────────────

    #[test]
    fn scan_streaming_unclosed_no_hit() {
        // F1-F4：未闭合阶段恒无命中；F5 闭合 `)` 出现即命中
        for src in ["!", "![alt]", "![alt](", "![alt](url"] {
            assert!(scan_images(src).is_empty(), "未闭合输入应无命中: {src:?}");
        }
        let hits = scan_images("![alt](url)");
        assert_eq!(hits.len(), 1, "F5 闭合即命中");
        assert_eq!(hits[0].alt, "alt");
        assert_eq!(hits[0].url, "url");
    }

    // ── C 组：alt 语义（S3 §4.1 表）───────────────────────────────

    #[test]
    fn scan_empty_alt() {
        // C5：空 alt
        let src = "![](u)";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].alt, "");
        assert_eq!(&src[hits[0].byte_start..hits[0].byte_end], "![](u)");
    }

    #[test]
    fn scan_alt_strips_markup() {
        // C1/C2：强调标记剥离、转义还原
        let bold = scan_images("![**bold**](u)");
        assert_eq!(bold[0].alt, "bold");

        let esc_paren = scan_images("![a\\(b](u)");
        assert_eq!(esc_paren[0].alt, "a(b", "转义圆括号还原（S3 §4.1 表）");
    }

    // ── E 组：代码上下文完全不命中（S3 E1-E4）────────────────────

    #[test]
    fn scan_code_context_no_hit() {
        assert!(scan_images("```\n![a](u)\n```").is_empty(), "E1 闭合围栏");
        assert!(scan_images("```\n![a](u)").is_empty(), "E2 未闭合围栏");
        assert!(scan_images("`![a](u)`").is_empty(), "E3 行内代码");
        assert!(scan_images("    ![a](u)").is_empty(), "E4 缩进代码块");
    }

    // ── D 组：dest 硬边界（S3 §4.3）───────────────────────────────

    #[test]
    fn scan_dest_bare_space_no_hit() {
        // D1/D3：dest 含裸空格（无 <> 包裹）不命中——CommonMark 硬边界
        // （destination 扫描到空白即停），属预期行为而非缺陷。
        assert!(scan_images("![a](my file.png)").is_empty(), "D1 裸空格");
        assert!(scan_images("![a](my\\ file.png)").is_empty(), "D3 转义空格");
        // D2：尖括号包裹则命中，url 去尖括号
        let hits = scan_images("![a](<my file.png>)");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "my file.png");
    }

    // ── P2-3：blockquote 内图片扫描命中 ───────────────────────────

    #[test]
    fn scan_blockquote() {
        let src = "> ![a](u)";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1, "blockquote 内图片应命中");
        assert_eq!(&src[hits[0].byte_start..hits[0].byte_end], "![a](u)");
    }

    // ── P2-4：多行 alt 的 softbreak 折叠补空格 ─────────────────────

    #[test]
    fn scan_multiline_alt_softbreak_joins_with_space() {
        // `![a\nb](u)`：softbreak 折叠语义为 "a b"（修复前拼成 "ab"）。
        let hits = scan_images("![a\nb](u)");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].alt, "a b", "softbreak 应折叠为空格");
    }

    // ── H 组：reference 形式（S3 §4.7）────────────────────────────

    #[test]
    fn scan_reference_needs_definition() {
        // 无定义：按字面文本，0 命中
        assert!(scan_images("![a][ref]").is_empty(), "无定义 shortcut 引用");

        // 有定义：命中且 id 非空、url 已从定义解析，区间覆盖 `![a][ref]`
        let src = "![a][ref]\n\n[ref]: https://example.com/x";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "ref");
        assert_eq!(hits[0].url, "https://example.com/x");
        assert_eq!(&src[hits[0].byte_start..hits[0].byte_end], "![a][ref]");
    }

    #[test]
    fn scan_title() {
        let hits = scan_images("![a](u \"t\")");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "t");
        assert_eq!(scan_images("![a](u)")[0].title, "", "无 title 为空串");
    }

    // ── 占位替换（S3 §7.2 + §4.5 碰撞解法）────────────────────────

    #[test]
    fn replace_multi_image_reverse_order() {
        // 逆序替换：区间基于原始文本，多图时 token 顺序与 side table 对应
        let src = "![a](u) ![b](v)";
        let hits = scan_images(src);
        let (replaced, infos) = replace_images(src, &hits);
        assert_eq!(replaced, "\u{0}IMG0\u{0} \u{0}IMG1\u{0}");
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].token, "\u{0}IMG0\u{0}");
        assert_eq!(infos[1].token, "\u{0}IMG1\u{0}");
        // 每个 token 恰好出现 1 次；无 `![` 残留
        assert_eq!(replaced.matches("\u{0}IMG0\u{0}").count(), 1);
        assert_eq!(replaced.matches("\u{0}IMG1\u{0}").count(), 1);
        assert!(!replaced.contains("!["), "替换后不应残留图片语法");
        // side table 与 hits 顺序一致（token 与区间一一对应）
        assert_eq!(infos[0].byte_start, hits[0].byte_start);
        assert_eq!(infos[1].byte_start, hits[1].byte_start);
    }

    #[test]
    fn replace_collision_free_renumber() {
        // G1：用户文本已含 \u{0}IMG0\u{0} → 图片自动获得不碰撞编号
        let src = "复制了 \u{0}IMG0\u{0} 然后 ![a](u)";
        let hits = scan_images(src);
        assert_eq!(hits.len(), 1);
        let (replaced, infos) = replace_images(src, &hits);
        assert_eq!(infos[0].token, "\u{0}IMG1\u{0}", "应跳过已占用编号 0");
        assert_eq!(
            replaced.matches("\u{0}IMG1\u{0}").count(),
            1,
            "新 token 恰好 1 次"
        );
        assert_eq!(
            replaced.matches("\u{0}IMG0\u{0}").count(),
            1,
            "用户文本保持 1 次"
        );
        assert!(!replaced.contains("!["), "替换后不应残留图片语法");
    }

    #[test]
    fn replace_unknown_number_no_collision() {
        // G3：用户文本含不存在编号 IMG999 → 图片仍分配 IMG0，互不冲突
        let src = "复制了 \u{0}IMG999\u{0} 然后 ![a](u)";
        let (replaced, infos) = replace_images(src, &scan_images(src));
        assert_eq!(infos[0].token, "\u{0}IMG0\u{0}");
        assert_eq!(replaced.matches("\u{0}IMG0\u{0}").count(), 1);
        assert_eq!(replaced.matches("\u{0}IMG999\u{0}").count(), 1);
    }

    #[test]
    fn replace_empty_alt_and_no_hits() {
        // 空 alt 替换为 token；无命中时返回原文（替换为无操作）
        let (replaced, infos) = replace_images("![](u)", &scan_images("![](u)"));
        assert_eq!(replaced, "\u{0}IMG0\u{0}");
        assert_eq!(infos[0].alt, "");
        assert_eq!(infos[0].token, "\u{0}IMG0\u{0}");

        let (same, infos) = replace_images("plain text", &[]);
        assert_eq!(same, "plain text");
        assert!(infos.is_empty());
    }
}
