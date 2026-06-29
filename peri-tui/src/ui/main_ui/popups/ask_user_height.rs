use peri_middlewares::ask_user::AskUserQuestionData; // P4b: type-dependency

/// 编号数字的十进制宽度
fn digit_width(n: usize) -> u16 {
    if n == 0 {
        return 1;
    }
    let mut d = 0u16;
    let mut v = n;
    while v > 0 {
        d += 1;
        v /= 10;
    }
    d
}

/// 精确计算 AskUser 弹窗内容行数，与 render_ask_user_popup 的 lines 结构 1:1 对齐。
///
/// 返回值包含 header(1) + 分隔线(1) + BorderedPanel 上下边框(2) 的开销。
pub(crate) fn ask_user_content_height(q: &AskUserQuestionData, panel_width: usize) -> u16 {
    let w = panel_width.max(1) as u16;
    let mut lines: u16 = 0;

    // 问题文本（考虑自动换行）
    for line in q.question.lines() {
        let line_w = unicode_width::UnicodeWidthStr::width(line) as u16;
        lines += line_w.div_ceil(w);
    }
    // 问题后空行
    lines += 1;

    let multi = q.multi_select;
    let option_count = q.options.len();

    for (i, opt) in q.options.iter().enumerate() {
        let num = i + 1;
        // 光标行前缀最宽：❯(2) + " "(1) + [○/●](1) + " "(1) + digits + ". "(2)
        // 非光标行前缀窄 1 列（用空格代替 ❯），但按最宽（光标行）估算
        let prefix_w: u16 = if multi {
            2 + 1 + 1 + 1 + digit_width(num) + 2
        } else {
            2 + 1 + digit_width(num) + 2
        };
        let label_w = unicode_width::UnicodeWidthStr::width(opt.label.as_str()) as u16 + prefix_w;
        lines += label_w.div_ceil(w);

        // 选项 description 行（若有）
        if let Some(ref desc) = opt.description {
            if !desc.is_empty() {
                let indent_w: u16 = if multi { 7 } else { 5 };
                let desc_w = unicode_width::UnicodeWidthStr::width(desc.as_str()) as u16 + indent_w;
                lines += desc_w.div_ceil(w);
            }
        }

        // 选项之间空行（最后一个选项不加）
        if i < option_count - 1 {
            lines += 1;
        }
    }

    // 自定义输入前空行
    lines += 1;

    // 自定义输入行（和单选选项前缀格式相同，custom_num = option_count + 1）
    {
        let custom_num = option_count + 1;
        let prefix_w: u16 = 2 + 1 + digit_width(custom_num) + 2;
        // placeholder 文本通常很短，但为精确起见按最大宽度算
        // 非光标时显示 placeholder "Type something." / "输入自定义内容..."
        // 光标时显示用户输入（可能很长），按 placeholder 宽度估算
        let placeholder = "输入自定义内容..."; // zh-CN placeholder 更长
        let text_w = unicode_width::UnicodeWidthStr::width(placeholder) as u16 + prefix_w;
        lines += text_w.div_ceil(w).max(1);
    }

    // header tab 行 + 分隔线 + BorderedPanel 上下边框 = 4
    lines + 4
}
