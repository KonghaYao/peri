//! S3 spike：markdown 前置扫描 + 占位 token 替换方案验证（§8.3 第 4 项）。
//!
//! 运行：`cargo run -p md-scan-matrix`（在 side-projects/md-scan-matrix 下）。
//! 输出：矩阵结果表（供写入 .peri/plans/spike-scan-matrix.md）。

mod matrix;
mod scanner;

use matrix::{Case, Outcome, evaluate, matrix_a, matrix_b, matrix_c, matrix_d, matrix_e, matrix_g};
use scanner::{
    TokenKind, block_rows, count_occurrences, parse, replace_collision_free, replace_images,
    scan_images,
};

fn main() {
    println!("=== md-scan-matrix: 前置扫描 + 占位 token 替换实验 ===\n");

    let mut cases: Vec<Case> = Vec::new();
    cases.extend(matrix_a());
    cases.extend(matrix_b());
    cases.extend(matrix_c());
    cases.extend(matrix_d());
    cases.extend(matrix_e());
    cases.extend(matrix_g());

    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut fail = 0usize;
    for c in &cases {
        let o = evaluate(c);
        if !o.pass() {
            fail += 1;
        }
        outcomes.push(o);
    }

    // ── 矩阵结果表 ──
    println!("## 矩阵 A–E/G 结果\n");
    println!(
        "{:<34} | {:<4} | {:<4} | {:<5} | {:<7} | {:<7} | {:<9} | {:<9} | {:<7} | 结论",
        "case", "hit期望", "hit实际", "切片", "NUL结构", "PUA结构", "NUL保留", "PUA保留", "无残留"
    );
    println!("{}", "-".repeat(140));
    for o in &outcomes {
        let hits_ok = o.actual_hits == o.expect_hits;
        println!(
            "{:<34} | {:<4} | {:<4} | {:<5} | {:<7} | {:<7} | {:<9} | {:<9} | {:<7} | {}",
            o.name,
            o.expect_hits,
            o.actual_hits,
            ok(slice_ok_str(o)),
            ok(o.shape_nul),
            ok(o.shape_pua),
            ok(o.nul_retained),
            ok(o.pua_retained),
            ok(o.leftover_ok),
            if o.pass() { "PASS" } else { "FAIL" }
        );
        if !hits_ok {
            println!("    ↳ 命中摘要: {}", o.hit_summary);
        }
        for d in &o.detail {
            println!("    ↳ {d}");
        }
    }
    println!("\n矩阵合计: {} cases, {} FAIL\n", outcomes.len(), fail);

    // ── 每 case 的命中详情（alt/url/title/区间切片）──
    println!("## 命中详情（side table 内容）\n");
    for o in &outcomes {
        let note = cases.iter().find(|c| c.name == o.name).map(|c| c.note).unwrap_or("");
        println!("- {} [{}]: {}", o.name, note, if o.slices.is_empty() { "(无命中)".into() } else { o.slices.join(", ") });
        if !o.hit_summary.is_empty() {
            println!("  ↳ {}\n", o.hit_summary);
        }
    }

    streaming_check();
    collision_resolve_check();
    reference_image_note();
}

fn ok(b: bool) -> &'static str {
    if b { "✓" } else { "✗" }
}

fn slice_ok_str(o: &Outcome) -> bool {
    o.slice_ok
}

// ── F 组：流式序列 ─────────────────────────────────────────────

fn streaming_check() {
    println!("\n## F 组：流式序列 `!` → `![alt]` → `![alt](` → `![alt](url` → `![alt](url)`\n");
    let stages = [
        ("F1", "!"),
        ("F2", "![alt]"),
        ("F3", "![alt]("),
        ("F4", "![alt](url"),
        ("F5", "![alt](url)"),
    ];
    let mut all_ok = true;
    for (id, md) in stages {
        let hits = scan_images(md);
        let substituted = replace_images(md, &hits, TokenKind::Nul);
        let substituted_plain = replace_images(md, &hits, TokenKind::Plain);
        let shape_of = |blocks: &[ratatui_kit_markdown::ParsedBlock]| -> Vec<(String, Vec<usize>)> {
            blocks.iter().map(scanner::shape).collect()
        };
        let s1 = shape_of(&parse(&substituted));
        let s2 = shape_of(&parse(&substituted_plain));
        // 无命中 ⇔ 替换无操作（hits>0 时替换必然改变文本，反之亦然）
        let noop_ok = (hits.is_empty()) == (substituted == md);
        let shape_ok = s1 == s2;
        if !noop_ok || !shape_ok {
            all_ok = false;
        }
        println!(
            "{} `{:?}`: hits={} 替换语义正确={} 结构一致={} {}",
            id, md, hits.len(), ok(noop_ok), ok(shape_ok),
            if noop_ok && shape_ok { "PASS" } else { "FAIL" }
        );
        println!("    ↳ 替换后 parse 结构: {:?}", s1);
    }

    // 多段前缀场景：图片出现在第二个段落，验证已闭合前缀块不受影响
    println!();
    let multi = [
        "intro\n\n![a](",
        "intro\n\n![a](u",
        "intro\n\n![a](u)",
    ];
    let mut prev: Option<Vec<(String, Vec<usize>)>> = None;
    for (i, md) in multi.iter().enumerate() {
        let hits = scan_images(md);
        let sub = replace_images(md, &hits, TokenKind::Nul);
        let s: Vec<(String, Vec<usize>)> = parse(&sub).iter().map(scanner::shape).collect();
        // 前缀稳定性：排除流式生长的最后一段（它必然变化），比较已闭合前缀块
        let prefix_stable = match &prev {
            Some(p) => {
                let n = p.len().saturating_sub(1);
                p[..n] == s[..n]
            }
            None => true,
        };
        println!(
            "流式多段 stage{} `{:?}`: hits={} 块数={} 已闭合前缀(前{}块)稳定={}",
            i + 1,
            md,
            hits.len(),
            s.len(),
            prev.as_ref().map(|p| p.len().saturating_sub(1)).unwrap_or(0),
            ok(prefix_stable)
        );
        println!("    ↳ 结构: {:?}", s);
        prev = Some(s);
    }
    if all_ok {
        println!("\nF 组结论: 全部阶段 PASS");
    } else {
        println!("\nF 组结论: 存在 FAIL，见上");
    }
}

// ── G 组补充：碰撞解决 ─────────────────────────────────────────

fn collision_resolve_check() {
    println!("\n## G 组补充：碰撞检测与重编号\n");

    // G1: 用户文本已含 NUL token → 朴素替换产生歧义
    let md1 = "复制了 \u{0}IMG0\u{0} 然后 ![a](u)";
    let hits1 = scan_images(md1);
    let naive = replace_images(md1, &hits1, TokenKind::Nul);
    let cnt_naive = count_occurrences(&naive, "\u{0}IMG0\u{0}");
    println!(
        "G1 朴素替换: hits={}, 替换后 `\\u{{0}}IMG0\\u{{0}}` 出现 {} 次 (应为 1) → {}",
        hits1.len(),
        cnt_naive,
        if cnt_naive == 1 { "无冲突" } else { "冲突（用户文本被误判为占位）" }
    );

    // 重编号解决：图片获得源文本中不存在的编号
    let (fixed, tokens) = replace_collision_free(md1, &hits1, TokenKind::Nul);
    let all_unique = tokens
        .iter()
        .all(|t| count_occurrences(&fixed, t) == 1);
    println!(
        "G1 重编号替换: tokens={:?}, 每 token 恰好出现 1 次={} → {}",
        tokens,
        ok(all_unique),
        if all_unique { "冲突解决" } else { "仍冲突" }
    );

    // G2: 用户文本含不存在编号 → 查表 miss 语义
    let md2 = "复制了 \u{0}IMG999\u{0} 然后 ![a](u)";
    let hits2 = scan_images(md2);
    let (fixed2, tokens2) = replace_collision_free(md2, &hits2, TokenKind::Nul);
    println!(
        "G2 不存在编号: 图片编号={:?}, 用户串 IMG999 保持原样={}",
        tokens2,
        ok(fixed2.contains("\u{0}IMG999\u{0}"))
    );

    // G3: PUA 同样冲突可解
    let md3 = "复制了 \u{E000}IMG0\u{E000} 然后 ![a](u)";
    let hits3 = scan_images(md3);
    let (fixed3, tokens3) = replace_collision_free(md3, &hits3, TokenKind::Pua);
    let unique3 = tokens3
        .iter()
        .all(|t| count_occurrences(&fixed3, t) == 1);
    println!(
        "G3 PUA 重编号: tokens={:?}, 唯一={} → {}",
        tokens3,
        ok(unique3),
        if unique3 { "冲突解决" } else { "仍冲突" }
    );

    // G4: 用户文本为裸词 IMG0 → 与 Plain token 必然碰撞（说明必须包裹）
    let md4 = "复制了 IMG0 然后 ![a](u)";
    let hits4 = scan_images(md4);
    let naive4 = replace_images(md4, &hits4, TokenKind::Plain);
    let cnt4 = count_occurrences(&naive4, "IMG0");
    println!(
        "G4 裸 ASCII 词: 替换后 `IMG0` 出现 {} 次 → {}",
        cnt4,
        if cnt4 == 1 { "恰好一次（侥幸）" } else { "碰撞（Plain 形式不可用）" }
    );

    // G5: 代码块内的 token 串（渲染时原样显示，不查表）
    let md5 = "```\n\u{0}IMG0\u{0}\n```";
    let hits5 = scan_images(md5);
    let sub5 = replace_images(md5, &hits5, TokenKind::Nul);
    let blocks5 = parse(&sub5);
    println!(
        "G5 代码块内 token 串: hits={}（不应扫描为图片），CodeBlock 内容保留: {:?}",
        hits5.len(),
        blocks5
            .iter()
            .map(block_rows)
            .collect::<Vec<_>>()
    );
}

// ── reference 形式观察 ─────────────────────────────────────────

fn reference_image_note() {
    println!("\n## H 组：reference 形式观察（超出矩阵，仅记录）\n");

    // H1: 有定义的 reference image
    let md = "![a][ref]\n\n[ref]: url";
    let hits = scan_images(md);
    println!(
        "H1 `![a][ref]` + 定义: hits={} 摘要={:?}",
        hits.len(),
        hits.iter().map(|h| (h.byte_start, h.byte_end, h.alt.as_str(), h.url.as_str(), h.id.as_str())).collect::<Vec<_>>()
    );
    for h in &hits {
        println!("    ↳ 区间切片: {:?}", &md[h.byte_start..h.byte_end]);
    }

    // H2: 无定义的 shortcut reference
    let md2 = "![a][ref]";
    let hits2 = scan_images(md2);
    println!("H2 `![a][ref]` 无定义: hits={}（预期 0，按字面文本处理）", hits2.len());

    // H3: 无定义的 shortcut（短引用 `![a]`）
    let md3 = "![a]";
    let hits3 = scan_images(md3);
    println!("H3 `![a]` 无定义: hits={}（预期 0）", hits3.len());

    // H4: 同段多 reference + inline 混排
    let md4 = "![a][ref] ![b](u)\n\n[ref]: v";
    let hits4 = scan_images(md4);
    println!(
        "H4 混排: hits={} 摘要={:?}",
        hits4.len(),
        hits4.iter().map(|h| (h.alt.as_str(), h.url.as_str(), h.id.as_str())).collect::<Vec<_>>()
    );
}
