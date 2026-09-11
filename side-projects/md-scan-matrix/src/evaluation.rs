//! 实验语义评估；预期负例精确匹配，所有组共同决定进程退出码。

use crate::matrix::{self, Case};
use crate::scanner::{
    block_rows, count_occurrences, parse, replace_collision_free, replace_images, scan_images,
    token, TokenKind,
};
use ratatui_kit_markdown::ParsedBlock;

/// 单 case 结果。
pub struct Outcome {
    pub name: String,
    pub note: String,
    pub expected_collision: bool,
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
    // 碰撞 case 仅指定 token 的编号 0 预期两次；缺失/更多重复仍失败。
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
    let nul_retained = tokens_match_expectation(&nul_text, hits.len(), TokenKind::Nul, c.collide);
    let pua_retained = tokens_match_expectation(&pua_text, hits.len(), TokenKind::Pua, c.collide);

    // 区间切片 + 残留（expect_hits==0 的 case 是「图片语法原样保留」场景，
    // 如代码块内/字面文本回退，`![` 残留是正确行为，不检查）
    let mut slices = Vec::new();
    let mut slice_ok = true;
    for h in &hits {
        let Some(sl) = md.get(h.byte_start..h.byte_end) else {
            slice_ok = false;
            continue;
        };
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
        .map(|h| {
            format!(
                "alt={:?} url={:?} title={:?} id={:?}",
                h.alt, h.url, h.title, h.id
            )
        })
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
        note: c.note.to_string(),
        expected_collision: c.collide.is_some(),
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

fn tokens_match_expectation(
    text: &str,
    hits: usize,
    kind: TokenKind,
    collision: Option<TokenKind>,
) -> bool {
    (0..hits).all(|i| {
        let expected = if collision == Some(kind) && i == 0 {
            2
        } else {
            1
        };
        count_occurrences(text, &token(kind, i)) == expected
    })
}

pub struct Check {
    pub name: String,
    pub passed: bool,
    pub expected_collision: bool,
    pub detail: String,
}

impl Check {
    fn condition(name: &str, passed: bool, detail: String) -> Self {
        Self {
            name: name.into(),
            passed,
            expected_collision: false,
            detail,
        }
    }

    fn count(name: &str, actual: usize, expected: usize, expected_collision: bool) -> Self {
        Self {
            name: name.into(),
            passed: actual == expected,
            expected_collision,
            detail: format!("期望 {expected}，实际 {actual}"),
        }
    }
}

pub struct Experiment {
    pub matrix: Vec<Outcome>,
    pub streaming: Vec<Check>,
    pub collisions: Vec<Check>,
    pub references: Vec<Check>,
}

impl Experiment {
    pub fn failures(&self) -> usize {
        self.matrix.iter().filter(|o| !o.pass()).count()
            + self
                .streaming
                .iter()
                .chain(&self.collisions)
                .chain(&self.references)
                .filter(|c| !c.passed)
                .count()
    }

    pub fn exit_code(&self) -> u8 {
        let complete = !self.matrix.is_empty()
            && !self.streaming.is_empty()
            && !self.collisions.is_empty()
            && !self.references.is_empty();
        u8::from(!complete || self.failures() != 0)
    }
}

pub fn run() -> Experiment {
    let cases = [
        matrix::matrix_a(),
        matrix::matrix_b(),
        matrix::matrix_c(),
        matrix::matrix_d(),
        matrix::matrix_e(),
        matrix::matrix_g(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    Experiment {
        matrix: cases.iter().map(evaluate).collect(),
        streaming: streaming_checks(),
        collisions: collision_checks(),
        references: reference_checks(),
    }
}

type Shape = (String, Vec<usize>);

fn closed_prefix_stable(previous: &[Shape], current: &[Shape]) -> bool {
    let n = previous.len().saturating_sub(1);
    current.get(..n) == previous.get(..n)
}

fn prefix_check(name: &str, previous: &[Shape], current: &[Shape]) -> Check {
    Check::condition(
        name,
        closed_prefix_stable(previous, current),
        format!(
            "已闭合前缀 {} 块；当前结构 {current:?}",
            previous.len().saturating_sub(1)
        ),
    )
}

fn streaming_checks() -> Vec<Check> {
    let stages = [
        ("F1", "!", 0),
        ("F2", "![alt]", 0),
        ("F3", "![alt](", 0),
        ("F4", "![alt](url", 0),
        ("F5", "![alt](url)", 1),
    ];
    let mut checks = Vec::new();
    for (id, md, expected_hits) in stages {
        let hits = scan_images(md);
        let substituted = replace_images(md, &hits, TokenKind::Nul);
        let plain = replace_images(md, &hits, TokenKind::Plain);
        let nul_shape: Vec<_> = parse(&substituted)
            .iter()
            .map(crate::scanner::shape)
            .collect();
        let plain_shape: Vec<_> = parse(&plain).iter().map(crate::scanner::shape).collect();
        checks.push(Check::count(
            &format!("{id} 命中"),
            hits.len(),
            expected_hits,
            false,
        ));
        checks.push(Check::condition(
            &format!("{id} 替换语义"),
            hits.is_empty() == (substituted == md),
            format!("输入 {md:?}，替换后 {substituted:?}"),
        ));
        checks.push(Check::condition(
            &format!("{id} 结构"),
            nul_shape == plain_shape,
            format!("NUL {nul_shape:?}；Plain {plain_shape:?}"),
        ));
    }
    let mut previous = Vec::new();
    for (i, md) in ["intro\n\n![a](", "intro\n\n![a](u", "intro\n\n![a](u)"]
        .iter()
        .enumerate()
    {
        let hits = scan_images(md);
        let substituted = replace_images(md, &hits, TokenKind::Nul);
        let current = parse(&substituted)
            .iter()
            .map(crate::scanner::shape)
            .collect::<Vec<_>>();
        checks.push(prefix_check(
            &format!("F 多段 stage{}", i + 1),
            &previous,
            &current,
        ));
        previous = current;
    }
    checks
}

fn collision_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    for (name, kind) in [
        ("G NUL", TokenKind::Nul),
        ("G PUA", TokenKind::Pua),
        ("G Plain", TokenKind::Plain),
    ] {
        let user_token = token(kind, 0);
        let md = format!("复制了 {user_token} 然后 ![a](u)");
        let hits = scan_images(&md);
        checks.push(Check::count(&format!("{name} 命中"), hits.len(), 1, false));
        let naive = replace_images(&md, &hits, kind);
        checks.push(Check::count(
            &format!("{name} 朴素替换碰撞"),
            count_occurrences(&naive, &user_token),
            2,
            true,
        ));
        let (fixed, tokens) = replace_collision_free(&md, &hits, kind);
        checks.push(resolved_collision_check(
            &format!("{name} 重编号"),
            &md,
            &fixed,
            &tokens,
            hits.len(),
            &user_token,
        ));
    }
    let user_token = token(TokenKind::Nul, 999);
    let md = format!("复制了 {user_token} 然后 ![a](u)");
    let hits = scan_images(&md);
    let (fixed, tokens) = replace_collision_free(&md, &hits, TokenKind::Nul);
    checks.push(Check::condition(
        "G 不存在编号保留",
        hits.len() == 1 && collision_resolved(&md, &fixed, &tokens, hits.len(), &user_token),
        format!("tokens={tokens:?}；用户串={user_token:?}；结果={fixed:?}"),
    ));
    let md = "```\n\u{0}IMG0\u{0}\n```";
    let hits = scan_images(md);
    let substituted = replace_images(md, &hits, TokenKind::Nul);
    let blocks = parse(&substituted);
    let code_preserved = matches!(blocks.as_slice(), [ParsedBlock::CodeBlock(_, lines)]
        if lines.join("\n") == "\u{0}IMG0\u{0}");
    checks.push(Check::condition(
        "G 代码块中的 token 不参与替换",
        hits.is_empty() && substituted == md && code_preserved,
        format!(
            "hits={}；blocks={:?}",
            hits.len(),
            blocks.iter().map(block_rows).collect::<Vec<_>>()
        ),
    ));
    checks
}

fn resolved_collision_check(
    name: &str,
    source: &str,
    fixed: &str,
    tokens: &[String],
    hits: usize,
    user_token: &str,
) -> Check {
    Check::condition(
        name,
        collision_resolved(source, fixed, tokens, hits, user_token),
        format!("tokens={tokens:?}；替换结果={fixed:?}"),
    )
}

fn collision_resolved(
    source: &str,
    fixed: &str,
    tokens: &[String],
    hits: usize,
    user_token: &str,
) -> bool {
    tokens.len() == hits
        && tokens.iter().enumerate().all(|(i, t)| {
            !source.contains(t) && !tokens[..i].contains(t) && count_occurrences(fixed, t) == 1
        })
        && count_occurrences(fixed, user_token) == count_occurrences(source, user_token)
}

fn reference_checks() -> Vec<Check> {
    let cases = [
        (
            "H1 有定义 reference",
            "![a][ref]\n\n[ref]: url",
            vec![("a", "url", "ref")],
        ),
        ("H2 未定义 reference", "![a][ref]", vec![]),
        ("H3 未定义 shortcut", "![a]", vec![]),
        (
            "H4 reference/inline 混排",
            "![a][ref] ![b](u)\n\n[ref]: v",
            vec![("a", "v", "ref"), ("b", "u", "")],
        ),
    ];
    cases
        .into_iter()
        .map(|(name, md, expected)| {
            let hits = scan_images(md);
            let actual: Vec<_> = hits
                .iter()
                .map(|h| (h.alt.as_str(), h.url.as_str(), h.id.as_str()))
                .collect();
            let slices_valid = hits.iter().all(|h| {
                md.get(h.byte_start..h.byte_end)
                    .is_some_and(|slice| slice.starts_with("!["))
            });
            Check::condition(
                name,
                actual == expected && slices_valid,
                format!("期望 {expected:?}；实际 {actual:?}；切片有效={slices_valid}"),
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "evaluation_test.rs"]
mod tests;
