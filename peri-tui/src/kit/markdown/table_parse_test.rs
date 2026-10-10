use super::*;

const TEST_BASE_FG: Color = Color::White;

/// 辅助：验证表格 header + data rows 在渲染后的 lines 中都可见。
fn assert_table_contains(md: &str, width: usize, headers: &[&str], rows: &[&[&str]], label: &str) {
    use ratatui_kit::components::TableTheme;

    let segments = parse_markdown(md, width, Palette::default(), TEST_BASE_FG);
    let table_data = segments
        .iter()
        .find_map(|s| match s {
            MarkdownSegment::Table(d) => Some(d),
            _ => None,
        })
        .unwrap_or_else(|| panic!("[{label}] 应解析出 Table segment"));

    let expected_header_count = headers.len();
    let expected_row_count = rows.len();
    assert_eq!(
        table_data.headers.len(),
        expected_header_count,
        "[{label}] 表头应有 {expected_header_count} 列，实际 {}",
        table_data.headers.len()
    );
    assert_eq!(
        table_data.rows.len(),
        expected_row_count,
        "[{label}] 应有 {expected_row_count} 行数据，实际 {}",
        table_data.rows.len()
    );

    let theme = TableTheme::from_palette(&Palette::default());
    let lines = table_data_to_lines(table_data, &theme, width);

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();

    // 验证每个表头
    for h in headers {
        assert!(
            all_text.contains(h),
            "[{label}] 应包含表头 '{h}'，实际文本:\n{all_text}"
        );
    }
    // 验证每个数据单元格
    for (ri, row) in rows.iter().enumerate() {
        for cell in *row {
            assert!(
                all_text.contains(cell),
                "[{label}] 应包含数据行{ri}单元格 '{cell}'，实际文本:\n{all_text}"
            );
        }
    }
    // 验证至少渲染了足够行数（顶边框 + 表头行 + 分隔行 + N行数据 + 底边框）
    let min_expected_lines =
        2 /* top+bottom border */ + 1 /* header */ + 1 /* separator */+ rows.len();
    assert!(
        lines.len() >= min_expected_lines,
        "[{label}] 渲染行数 {} 少于预期最小值 {min_expected_lines}",
        lines.len()
    );
}

#[test]
fn test_table_1_simple_cjk() {
    // 1️⃣ 简单中文表
    assert_table_contains(
        "| 列A | 列B |\n|------|------|\n| 值1 | 值2 |\n| 数据 | 表格 |\n",
        80,
        &["列A", "列B"],
        &[&["值1", "值2"], &["数据", "表格"]],
        "1-simple-cjk",
    );
}

#[test]
fn test_table_2_long_cjk_content() {
    // 2️⃣ 长中文内容
    assert_table_contains(
        "| 文件名 | 说明 |\n|--------|------|\n| 这是一个很长的中文文件名示例 | 核心改动——修改了表格渲染管线 |\n",
        80,
        &["文件名", "说明"],
        &[&[
            "这是一个很长的中文文件名示例",
            "核心改动——修改了表格渲染管线",
        ]],
        "2-long-cjk",
    );
}

#[test]
fn test_table_3_mixed_cjk_ascii() {
    // 3️⃣ 中英混合
    assert_table_contains(
        "| File Path | 改动类型 |\n|-----------|----------|\n| table.rs | 修改核心逻辑 |\n| types.rs | 新增数据结构 |\n",
        80,
        &["File Path", "改动类型"],
        &[&["table.rs", "修改核心逻辑"], &["types.rs", "新增数据结构"]],
        "3-mixed-cjk-ascii",
    );
}

#[test]
fn test_table_4_inline_code_in_cells() {
    // 4️⃣ 单元格含行内代码
    assert_table_contains(
        "| 函数 | 说明 |\n|------|------|\n| `foo()` | 执行某某操作 |\n| `bar()` | 另一操作 |\n",
        80,
        &["函数", "说明"],
        &[&["foo", "执行某某操作"], &["bar", "另一操作"]],
        "4-inline-code",
    );
}

#[test]
fn test_table_5_narrow_width() {
    // 5️⃣ 窄宽度（40列）
    assert_table_contains(
        "| 列A | 列B |\n|------|------|\n| 值1 | 值2 |\n",
        40,
        &["列A", "列B"],
        &[&["值1", "值2"]],
        "5-narrow-40",
    );
}

#[test]
fn test_table_6_three_columns() {
    // 6️⃣ 三列中英表
    assert_table_contains(
        "| 模块 | 状态 | 备注 |\n|------|------|------|\n| peri-tui | 已完成 | 主要前端 |\n| peri-agent | 进行中 | 核心引擎 |\n| peri-acp | 已完成 | 协议层 |\n",
        80,
        &["模块", "状态", "备注"],
        &[
            &["peri-tui", "已完成", "主要前端"],
            &["peri-agent", "进行中", "核心引擎"],
            &["peri-acp", "已完成", "协议层"],
        ],
        "6-three-cols",
    );
}

#[test]
fn test_table_7_many_rows() {
    // 7️⃣ 多行数据
    assert_table_contains(
        "| # | 项目 |\n|---|------|\n| 1 | 第一项 |\n| 2 | 第二项 |\n| 3 | 第三项 |\n| 4 | 第四项 |\n| 5 | 第五项 |\n",
        80,
        &["#", "项目"],
        &[
            &["1", "第一项"],
            &["2", "第二项"],
            &["3", "第三项"],
            &["4", "第四项"],
            &["5", "第五项"],
        ],
        "7-many-rows",
    );
}

#[test]
fn test_table_8_typical_agent_output() {
    // 8️⃣ 类 agent 输出格式
    assert_table_contains(
        "| 文件 | 改动类型 | 说明 |\n|------|----------|------|\n| `peri-tui/src/kit/markdown/table.rs` | 修改 | 核心渲染逻辑 |\n| `peri-tui/src/kit/markdown/types.rs` | 可能修改 | 新增字段 |\n",
        80,
        &["文件", "改动类型", "说明"],
        &[
            &["peri-tui", "修改", "核心渲染逻辑"],
            &["peri-tui", "可能修改", "新增字段"],
        ],
        "8-agent-output",
    );
}
