//! 实验输入与预期；评估在 evaluation，终端输出在 report。

use crate::scanner::TokenKind;

/// 单个实验 case。
pub struct Case {
    pub name: &'static str,
    pub md: &'static str,
    pub expect_hits: usize,
    pub note: &'static str,
    /// 预期与该 token 形式发生碰撞（用户文本中已含同形式 token）。
    /// 仅该形式的 IMG0 预期出现两次；缺失或更多重复仍为失败。
    pub collide: Option<TokenKind>,
}

// ── 实验矩阵定义 ────────────────────────────────────────────────

pub fn matrix_a() -> Vec<Case> {
    vec![
        Case {
            name: "A1 独立图片（独占段落）",
            md: "![alt](url)",
            expect_hits: 1,
            note: "最简形态",
            collide: None,
        },
        Case {
            name: "A2 独立图片段（前后有段落）",
            md: "before\n\n![a](u)\n\nafter",
            expect_hits: 1,
            note: "图片自成一段",
            collide: None,
        },
        Case {
            name: "A3 段落内图片（中置）",
            md: "before ![a](u) after",
            expect_hits: 1,
            note: "inline 中置",
            collide: None,
        },
        Case {
            name: "A4 段落内图片（行首）",
            md: "![a](u) and text",
            expect_hits: 1,
            note: "inline 行首",
            collide: None,
        },
        Case {
            name: "A5 段落内图片（行尾）",
            md: "text and ![a](u)",
            expect_hits: 1,
            note: "inline 行尾",
            collide: None,
        },
        Case {
            name: "A6 同段多图（空格分隔）",
            md: "![a](u) ![b](v)",
            expect_hits: 2,
            note: "多图同段",
            collide: None,
        },
        Case {
            name: "A7 同段多图（无空格）",
            md: "![a](u)![b](v)",
            expect_hits: 2,
            note: "紧密相邻",
            collide: None,
        },
        Case {
            name: "A8 多段多图",
            md: "![a](u)\n\n![b](v)\n\n![c](w)",
            expect_hits: 3,
            note: "每段一图",
            collide: None,
        },
        Case {
            name: "A9 图与文本交替",
            md: "t1 ![a](u) t2\n\n![b](v) t3",
            expect_hits: 2,
            note: "混合分布",
            collide: None,
        },
    ]
}

pub fn matrix_b() -> Vec<Case> {
    vec![
        Case {
            name: "B1 列表项内图片",
            md: "- ![a](u)",
            expect_hits: 1,
            note: "列表项为唯一内容",
            collide: None,
        },
        Case {
            name: "B2 列表项图文混排",
            md: "- item ![a](u) tail",
            expect_hits: 1,
            note: "列表项 inline",
            collide: None,
        },
        Case {
            name: "B3 有序列表",
            md: "1. ![a](u)\n2. ![b](v)",
            expect_hits: 2,
            note: "多有序项",
            collide: None,
        },
        Case {
            name: "B4 引用块内图片",
            md: "> ![a](u)",
            expect_hits: 1,
            note: "引用独占行",
            collide: None,
        },
        Case {
            name: "B5 引用图文混排",
            md: "> quote ![a](u) end",
            expect_hits: 1,
            note: "引用 inline",
            collide: None,
        },
        Case {
            name: "B6 表格单元格",
            md: "| ![a](u) | x |\n|---|---|\n| ![b](v) | y |",
            expect_hits: 2,
            note: "表头/表体各一图",
            collide: None,
        },
        Case {
            name: "B7 强调内图片",
            md: "*![a](u)* 与 **![b](v)**",
            expect_hits: 2,
            note: "em/strong 包裹",
            collide: None,
        },
        Case {
            name: "B8 链接内图片（alt 为链接文本）",
            md: "[![a](u)](v)",
            expect_hits: 1,
            note: "嵌套 link",
            collide: None,
        },
        Case {
            name: "B9 链接后紧跟图片",
            md: "[b](v) ![a](u)",
            expect_hits: 1,
            note: "link+image 相邻",
            collide: None,
        },
        Case {
            name: "B10 标题内图片",
            md: "# Title ![a](u)",
            expect_hits: 1,
            note: "heading inline",
            collide: None,
        },
    ]
}

pub fn matrix_c() -> Vec<Case> {
    vec![
        Case {
            name: "C1 alt 含强调标记",
            md: "![**bold**](u)",
            expect_hits: 1,
            note: "观察 alt 是否去标记",
            collide: None,
        },
        Case {
            name: "C2 alt 含转义括号",
            md: "![a\\(b](u)",
            expect_hits: 1,
            note: "转义圆括号",
            collide: None,
        },
        Case {
            name: "C3 alt 含转义方括号",
            md: "![a\\]b](u)",
            expect_hits: 1,
            note: "转义方括号",
            collide: None,
        },
        Case {
            name: "C4 alt 含未转义括号",
            md: "![a(b)](u)",
            expect_hits: 1,
            note: "括号不成对闭合",
            collide: None,
        },
        Case {
            name: "C5 alt 为空",
            md: "![](u)",
            expect_hits: 1,
            note: "空 alt",
            collide: None,
        },
        Case {
            name: "C6 alt 含链接",
            md: "![[b](v)](u)",
            expect_hits: 1,
            note: "alt 嵌套 link",
            collide: None,
        },
        Case {
            name: "C7 alt 含行内代码",
            md: "![`code`](u)",
            expect_hits: 1,
            note: "观察 alt 去反引号",
            collide: None,
        },
        Case {
            name: "C8 alt 多词带空格",
            md: "![hello world](u)",
            expect_hits: 1,
            note: "普通多词",
            collide: None,
        },
    ]
}

pub fn matrix_d() -> Vec<Case> {
    vec![
        Case {
            name: "D1 dest 含空格（无尖括号）",
            md: "![a](my file.png)",
            expect_hits: 0,
            note: "CommonMark 非法 dest，观察回退",
            collide: None,
        },
        Case {
            name: "D2 dest 含空格（尖括号包裹）",
            md: "![a](<my file.png>)",
            expect_hits: 1,
            note: "合法含空格 dest",
            collide: None,
        },
        Case {
            name: "D3 dest 转义空格",
            md: "![a](my\\ file.png)",
            expect_hits: 0,
            note: "\\ 空格非法（空格非标点不可转义），同样字面回退",
            collide: None,
        },
        Case {
            name: "D4 dest 含括号",
            md: "![a](u(x))",
            expect_hits: 1,
            note: "嵌套圆括号",
            collide: None,
        },
        Case {
            name: "D5 title 双引号含括号",
            md: "![a](u \"t(x)\")",
            expect_hits: 1,
            note: "title 双引号",
            collide: None,
        },
        Case {
            name: "D6 title 单引号",
            md: "![a](u 't')",
            expect_hits: 1,
            note: "title 单引号",
            collide: None,
        },
        Case {
            name: "D7 title 括号包裹",
            md: "![a](u (t))",
            expect_hits: 1,
            note: "title 圆括号",
            collide: None,
        },
        Case {
            name: "D8 title 转义引号",
            md: "![a](u \"t\\\"x\")",
            expect_hits: 1,
            note: "title 内转义",
            collide: None,
        },
        Case {
            name: "D9 空 destination",
            md: "![a]()",
            expect_hits: 1,
            note: "dest 为空串",
            collide: None,
        },
    ]
}

pub fn matrix_e() -> Vec<Case> {
    vec![
        Case {
            name: "E1 闭合围栏代码块内图片语法",
            md: "```\n![a](u)\n```",
            expect_hits: 0,
            note: "代码块不解析 inline",
            collide: None,
        },
        Case {
            name: "E2 未闭合围栏代码块内图片语法",
            md: "```\n![a](u)",
            expect_hits: 0,
            note: "流式常见：fence 未闭合到 EOF",
            collide: None,
        },
        Case {
            name: "E3 行内代码内图片语法",
            md: "`![a](u)`",
            expect_hits: 0,
            note: "inline code 不解析",
            collide: None,
        },
        Case {
            name: "E4 缩进代码块内图片语法",
            md: "    ![a](u)",
            expect_hits: 0,
            note: "缩进代码块",
            collide: None,
        },
    ]
}

pub fn matrix_g() -> Vec<Case> {
    vec![
        Case {
            name: "G1 用户文本含 NUL token（同编号）",
            md: "复制了 \u{0}IMG0\u{0} 然后 ![a](u)",
            expect_hits: 1,
            note: "观察冲突",
            collide: Some(TokenKind::Nul),
        },
        Case {
            name: "G2 用户文本含 PUA token（同编号）",
            md: "复制了 \u{E000}IMG0\u{E000} 然后 ![a](u)",
            expect_hits: 1,
            note: "观察冲突",
            collide: Some(TokenKind::Pua),
        },
        Case {
            name: "G3 用户文本含不存在的编号",
            md: "复制了 \u{0}IMG999\u{0} 然后 ![a](u)",
            expect_hits: 1,
            note: "查表 miss 场景",
            collide: None,
        },
        Case {
            name: "G4 用户文本为裸 ASCII 词",
            md: "复制了 IMG0 然后 ![a](u)",
            expect_hits: 1,
            note: "Plain 无包裹对照",
            collide: None,
        },
        Case {
            name: "G5 用户文本含 NUL 包裹的裸词（无编号段）",
            md: "复制了 \u{0}\u{0} 然后 ![a](u)",
            expect_hits: 1,
            note: "包裹形式但无编号",
            collide: None,
        },
    ]
}
