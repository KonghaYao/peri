// ─── Fork 指令模板（自 fork.rs 迁移，纯字符串函数） ────────────────────────

/// Build fork directive message for fork mode.
pub fn build_fork_directive(prompt: &str) -> String {
    format!(
        "<fork_directive>\n\
         You are a forked agent continuing from the parent conversation.\n\
         You have full access to the conversation history above.\n\
         \n\
         RULES:\n\
         1. Do NOT spawn sub-agents — execute directly using your tools\n\
         2. Do NOT ask questions — act on the directive below\n\
         3. Stay strictly within your assigned scope\n\
         4. Report structured facts, then stop\n\
         5. Keep your response under 500 words unless specified otherwise\n\
         \n\
         Output format:\n\
           Scope: <your assigned scope in one sentence>\n\
           Result: <the answer or key findings>\n\
           Key files: <relevant file paths>\n\
           Files changed: <list if you modified files>\n\
         </fork_directive>\n\n\
         {prompt}"
    )
}

/// Build bg-fork directive message for /bg command path.
pub fn build_bg_fork_directive(prompt: &str) -> String {
    // 防御性 XML 注入防护
    let sanitized = prompt.replace("</bg_fork_directive>", "<\u{200b}/bg_fork_directive>");
    format!(
        "<bg_fork_directive>\n\
         你是后台异步 Agent，从父会话 fork 而来。\n\
         你拥有完整的对话历史上下文。\n\
         \n\
         规则：\n\
         1. 禁止生成子 Agent — 直接使用工具执行\n\
         2. 禁止提问 — 按指令行动\n\
         3. 严格限定在分配范围内\n\
         4. 先给出结论，再补充说明\n\
         5. 除非特别说明，回复控制在 500 字以内\n\
         \n\
         输出格式：\n\
           结论: <核心结论或答案>\n\
           详细说明: <补充细节>\n\
           关键文件: <相关文件路径>\n\
           建议: <后续行动建议>\n\
         </bg_fork_directive>\n\n\
         {sanitized}"
    )
}

/// 构建 Prediction 指令模板（中文）。
/// 用于 agent 完成后预测用户下一步输入。
///
/// `current_title` 为会话当前标题（`None` 表示尚无标题）。注入后模型才能判断
/// 现有标题是否需要更新——不传则模型无从得知标题现状，会默认不输出 title 标记。
pub fn build_prediction_directive(current_title: Option<&str>) -> String {
    // 防御性 XML 注入防护（标题可能含闭合标签文本）
    let title_ctx = match current_title {
        Some(t) => {
            let sanitized = t.replace("</prediction_directive>", "<\u{200b}/prediction_directive>");
            format!("当前会话标题：\"{sanitized}\"")
        }
        None => "当前会话标题：（无）".to_string(),
    };
    format!(
        "<prediction_directive>\n\
         你是预测输入助手。根据对话上下文，预测用户下一步最可能在输入框中输入什么，\n\
         并同步维护会话元数据。\n\
         \n\
         {title_ctx}\n\
         \n\
         规则：\n\
         1. 默认输出一句预测文本（占位符），不要解释\n\
         2. 预测应该是自然的用户语言，像用户自己会打的那样\n\
         3. 不要加引号、前缀或格式\n\
         4. 长度控制在 5-30 个字\n\
         5. 如果无法判断，输出空字符串\n\
         \n\
         结构化标记（仅在对应信息有价值时输出，可同时输出多个）：\n\
         - <peri:title>新标题</peri:title>：当标题缺失、过时或与当前任务不符时，主动更新为精炼的当前任务标题；话题转变时应立即更新\n\
         - <peri:tag>标签</peri:tag>：检测到明确主题时打一个标签（如 bugfix、refactor）\n\
         - <peri:summary>一句话摘要</peri:summary>：给整个对话写一句简短摘要\n\
         示例：继续排查内存泄漏 <peri:title>排查内存泄漏</peri:title><peri:tag>bugfix</peri:tag>\n\
         示例（话题转变，标题应立即更新）：<peri:title>性能优化</peri:title>\n\
         </prediction_directive>"
    )
}
