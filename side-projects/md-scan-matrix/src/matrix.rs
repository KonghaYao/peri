//! 实验矩阵：§8.3 spike 清单第 4 项「前置扫描实验矩阵」。
//!
//! 每个 case 断言：
//! 1. 命中数 == 预期；
//! 2. 区间切片以 `![` 开头（映射回原始文本有效）；
//! 3. 替换后 parse 的**结构形状**与「裸 ASCII 词（Plain）参考替换」一致
//!    （NUL / PUA 两种 token 均不破坏块结构）；
//! 4. token 在解析后的 span 中完整保留（可查表映射回图片）；
//! 5. 替换后文本无 `![` 语法残留。

use crate::scanner::{
    TokenKind, block_rows, count_occurrences, parse, replace_images, scan_images, token,
};
use ratatui_kit_markdown::ParsedBlock;

/// 单个实验 case。
pub struct Case {
    pub name: &'static str,
    pub md: &'static str,
    pub expect_hits: usize,
    pub note: &'static str,
    /// 预期与该 token 形式发生碰撞（用户文本中已含同形式 token）。
    /// 命中后「保留断言」反转：碰撞发生 = 符合预期。
    pub collide: Option<TokenKind>,
}

/// 单 case 结果。
pub struct Outcome {
    pub name: String,
    pub expect_hits: usize,
    pub actual_hits: usize,
    pub slice_ok: bool,
    pub slices: Vec<String>,
    pub hit_summary: String,
    pub shape_nul: bool,
    pub shape_pua: bool,
    pub nul_retained: bool,
    pub pua_retained: bool,
    pub leftover_ok: bool,
    pub detail: Vec<String>,
}

impl Outcome {
    pub fn pass(&self) -> bool {
        self.actual_hits == self.expect_hits
            && self.slice_ok
            && self.shape_nul
            && self.shape_pua
            && self.nul_retained
            && self.pua_retained
            && self.leftover_ok
    }
}

/// 评估单个 case。
pub fn evaluate(c: &Case) -> Outcome {
    let md = c.md;
    let hits = scan_images(md);

    // 三种替换
    let nul = replace_images(md, &hits, TokenKind::Nul);
    let pua = replace_images(md, &hits, TokenKind::Pua);
    let plain = replace_images(md, &hits, TokenKind::Plain);

    let nul_blocks = parse(&nul);
    let pua_blocks = parse(&pua);
    let plain_blocks = parse(&plain);

    // 结构形状比较：kind 序列 + 每行 span 数
    let shape_of = |blocks: &[ParsedBlock]| -> Vec<(String, Vec<usize>)> {
        blocks.iter().map(crate::scanner::shape).collect()
    };
    let nul_shape = shape_of(&nul_blocks);
    let pua_shape = shape_of(&pua_blocks);
    let plain_shape = shape_of(&plain_blocks);
    let shape_nul = nul_shape == plain_shape;
    let shape_pua = pua_shape == plain_shape;

    // token 保留：解析后全文本应包含每种 token 恰好 hits 次。
    // 碰撞 case（collide == Some(kind)）断言反转：碰撞发生 = 符合预期。
    let all_text = |blocks: &[ParsedBlock]| -> String {
        let mut s = String::new();
        for b in blocks {
            let (_, rows) = block_rows(b);
            for row in rows {
                for sp in row {
                    s.push_str(&sp);
                }
                s.push('\n');
            }
        }
        s
    };
    let nul_text = all_text(&nul_blocks);
    let pua_text = all_text(&pua_blocks);
    let nul_retained_ok = (0..hits.len())
        .all(|i| count_occurrences(&nul_text, &token(TokenKind::Nul, i)) == 1);
    let pua_retained_ok = (0..hits.len())
        .all(|i| count_occurrences(&pua_text, &token(TokenKind::Pua, i)) == 1);
    let nul_retained = match c.collide {
        Some(TokenKind::Nul) => !nul_retained_ok,
        _ => nul_retained_ok,
    };
    let pua_retained = match c.collide {
        Some(TokenKind::Pua) => !pua_retained_ok,
        _ => pua_retained_ok,
    };

    // 区间切片 + 残留（expect_hits==0 的 case 是「图片语法原样保留」场景，
    // 如代码块内/字面文本回退，`![` 残留是正确行为，不检查）
    let mut slices = Vec::new();
    let mut slice_ok = true;
    for h in &hits {
        let sl = &md[h.byte_start..h.byte_end];
        slices.push(sl.to_string());
        if !sl.starts_with("![") {
            slice_ok = false;
        }
    }
    let leftover_ok = if c.expect_hits == 0 {
        true
    } else {
        !nul.contains("![") && !pua.contains("![")
    };

    let hit_summary = hits
        .iter()
        .map(|h| format!("alt={:?} url={:?} title={:?} id={:?}", h.alt, h.url, h.title, h.id))
        .collect::<Vec<_>>()
        .join(" | ");

    let mut detail = Vec::new();
    if !slice_ok || !shape_nul || !shape_pua || !nul_retained || !pua_retained || !leftover_ok {
        detail.push(format!("plain 结构: {:?}", plain_shape));
        detail.push(format!("nul   结构: {:?}", nul_shape));
        detail.push(format!("pua   结构: {:?}", pua_shape));
    }

    Outcome {
        name: c.name.to_string(),
        expect_hits: c.expect_hits,
        actual_hits: hits.len(),
        slice_ok,
        slices,
        hit_summary,
        shape_nul,
        shape_pua,
        nul_retained,
        pua_retained,
        leftover_ok,
        detail,
    }
}

// ── 实验矩阵定义 ────────────────────────────────────────────────

pub fn matrix_a() -> Vec<Case> {
    vec![
        Case { name: "A1 独立图片（独占段落）", md: "![alt](url)", expect_hits: 1, note: "最简形态", collide: None },
        Case { name: "A2 独立图片段（前后有段落）", md: "before\n\n![a](u)\n\nafter", expect_hits: 1, note: "图片自成一段", collide: None },
        Case { name: "A3 段落内图片（中置）", md: "before ![a](u) after", expect_hits: 1, note: "inline 中置", collide: None },
        Case { name: "A4 段落内图片（行首）", md: "![a](u) and text", expect_hits: 1, note: "inline 行首", collide: None },
        Case { name: "A5 段落内图片（行尾）", md: "text and ![a](u)", expect_hits: 1, note: "inline 行尾", collide: None },
        Case { name: "A6 同段多图（空格分隔）", md: "![a](u) ![b](v)", expect_hits: 2, note: "多图同段", collide: None },
        Case { name: "A7 同段多图（无空格）", md: "![a](u)![b](v)", expect_hits: 2, note: "紧密相邻", collide: None },
        Case { name: "A8 多段多图", md: "![a](u)\n\n![b](v)\n\n![c](w)", expect_hits: 3, note: "每段一图", collide: None },
        Case { name: "A9 图与文本交替", md: "t1 ![a](u) t2\n\n![b](v) t3", expect_hits: 2, note: "混合分布", collide: None },
    ]
}

pub fn matrix_b() -> Vec<Case> {
    vec![
        Case { name: "B1 列表项内图片", md: "- ![a](u)", expect_hits: 1, note: "列表项为唯一内容", collide: None },
        Case { name: "B2 列表项图文混排", md: "- item ![a](u) tail", expect_hits: 1, note: "列表项 inline", collide: None },
        Case { name: "B3 有序列表", md: "1. ![a](u)\n2. ![b](v)", expect_hits: 2, note: "多有序项", collide: None },
        Case { name: "B4 引用块内图片", md: "> ![a](u)", expect_hits: 1, note: "引用独占行", collide: None },
        Case { name: "B5 引用图文混排", md: "> quote ![a](u) end", expect_hits: 1, note: "引用 inline", collide: None },
        Case { name: "B6 表格单元格", md: "| ![a](u) | x |\n|---|---|\n| ![b](v) | y |", expect_hits: 2, note: "表头/表体各一图", collide: None },
        Case { name: "B7 强调内图片", md: "*![a](u)* 与 **![b](v)**", expect_hits: 2, note: "em/strong 包裹", collide: None },
        Case { name: "B8 链接内图片（alt 为链接文本）", md: "[![a](u)](v)", expect_hits: 1, note: "嵌套 link", collide: None },
        Case { name: "B9 链接后紧跟图片", md: "[b](v) ![a](u)", expect_hits: 1, note: "link+image 相邻", collide: None },
        Case { name: "B10 标题内图片", md: "# Title ![a](u)", expect_hits: 1, note: "heading inline", collide: None },
    ]
}

pub fn matrix_c() -> Vec<Case> {
    vec![
        Case { name: "C1 alt 含强调标记", md: "![**bold**](u)", expect_hits: 1, note: "观察 alt 是否去标记", collide: None },
        Case { name: "C2 alt 含转义括号", md: "![a\\(b](u)", expect_hits: 1, note: "转义圆括号", collide: None },
        Case { name: "C3 alt 含转义方括号", md: "![a\\]b](u)", expect_hits: 1, note: "转义方括号", collide: None },
        Case { name: "C4 alt 含未转义括号", md: "![a(b)](u)", expect_hits: 1, note: "括号不成对闭合", collide: None },
        Case { name: "C5 alt 为空", md: "![](u)", expect_hits: 1, note: "空 alt", collide: None },
        Case { name: "C6 alt 含链接", md: "![[b](v)](u)", expect_hits: 1, note: "alt 嵌套 link", collide: None },
        Case { name: "C7 alt 含行内代码", md: "![`code`](u)", expect_hits: 1, note: "观察 alt 去反引号", collide: None },
        Case { name: "C8 alt 多词带空格", md: "![hello world](u)", expect_hits: 1, note: "普通多词", collide: None },
    ]
}

pub fn matrix_d() -> Vec<Case> {
    vec![
        Case { name: "D1 dest 含空格（无尖括号）", md: "![a](my file.png)", expect_hits: 0, note: "CommonMark 非法 dest，观察回退", collide: None },
        Case { name: "D2 dest 含空格（尖括号包裹）", md: "![a](<my file.png>)", expect_hits: 1, note: "合法含空格 dest", collide: None },
        Case { name: "D3 dest 转义空格", md: "![a](my\\ file.png)", expect_hits: 0, note: "\\ 空格非法（空格非标点不可转义），同样字面回退", collide: None },
        Case { name: "D4 dest 含括号", md: "![a](u(x))", expect_hits: 1, note: "嵌套圆括号", collide: None },
        Case { name: "D5 title 双引号含括号", md: "![a](u \"t(x)\")", expect_hits: 1, note: "title 双引号", collide: None },
        Case { name: "D6 title 单引号", md: "![a](u 't')", expect_hits: 1, note: "title 单引号", collide: None },
        Case { name: "D7 title 括号包裹", md: "![a](u (t))", expect_hits: 1, note: "title 圆括号", collide: None },
        Case { name: "D8 title 转义引号", md: "![a](u \"t\\\"x\")", expect_hits: 1, note: "title 内转义", collide: None },
        Case { name: "D9 空 destination", md: "![a]()", expect_hits: 1, note: "dest 为空串", collide: None },
    ]
}

pub fn matrix_e() -> Vec<Case> {
    vec![
        Case { name: "E1 闭合围栏代码块内图片语法", md: "```\n![a](u)\n```", expect_hits: 0, note: "代码块不解析 inline", collide: None },
        Case { name: "E2 未闭合围栏代码块内图片语法", md: "```\n![a](u)", expect_hits: 0, note: "流式常见：fence 未闭合到 EOF", collide: None },
        Case { name: "E3 行内代码内图片语法", md: "`![a](u)`", expect_hits: 0, note: "inline code 不解析", collide: None },
        Case { name: "E4 缩进代码块内图片语法", md: "    ![a](u)", expect_hits: 0, note: "缩进代码块", collide: None },
    ]
}

pub fn matrix_g() -> Vec<Case> {
    vec![
        Case { name: "G1 用户文本含 NUL token（同编号）", md: "复制了 \u{0}IMG0\u{0} 然后 ![a](u)", expect_hits: 1, note: "观察冲突", collide: Some(TokenKind::Nul) },
        Case { name: "G2 用户文本含 PUA token（同编号）", md: "复制了 \u{E000}IMG0\u{E000} 然后 ![a](u)", expect_hits: 1, note: "观察冲突", collide: Some(TokenKind::Pua) },
        Case { name: "G3 用户文本含不存在的编号", md: "复制了 \u{0}IMG999\u{0} 然后 ![a](u)", expect_hits: 1, note: "查表 miss 场景", collide: None },
        Case { name: "G4 用户文本为裸 ASCII 词", md: "复制了 IMG0 然后 ![a](u)", expect_hits: 1, note: "Plain 无包裹对照", collide: None },
        Case { name: "G5 用户文本含 NUL 包裹的裸词（无编号段）", md: "复制了 \u{0}\u{0} 然后 ![a](u)", expect_hits: 1, note: "包裹形式但无编号", collide: None },
    ]
}
