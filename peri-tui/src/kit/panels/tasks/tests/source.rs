//! 面板源码约束（TUI-HOOK-001 / TUI-RENDER-001）与导航提示文案的一致性回归。

/// 面板组件源码。
fn tasks_source() -> &'static str {
    include_str!("../../tasks.rs")
}

/// 组件体：文件末尾的 `mod tests;` 之后只剩测试模块声明。
fn panel_source() -> &'static str {
    tasks_source()
        .split("mod tests;")
        .next()
        .expect("tasks 模块体存在")
}

/// TUI-HOOK-001：组件内全部 hook 在条件分支与渲染体之前；渲染体不写 atom、不起副作用。
#[test]
fn hooks_precede_conditionals_and_render() {
    let src = tasks_source();
    let body = src
        .split("pub fn TasksPanel")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("组件体存在");
    let last_hook = body.rfind("hooks.use_").expect("组件内有 hook 调用");
    let first_branch = [" if ", "match ", "for "]
        .iter()
        .filter_map(|needle| body.find(needle))
        .min()
        .expect("渲染体有控制流");
    assert!(
        last_hook < first_branch,
        "TUI-HOOK-001：hook 不得出现在条件分支或渲染体之后"
    );

    let render = &body[first_branch..];
    assert!(
        !render.contains("hooks.use_"),
        "渲染体不得再调用 hook（TUI-HOOK-001）"
    );
    assert!(
        !render.contains(".set("),
        "渲染体不得写 atom（TUI-RENDER-001）"
    );
    assert!(
        !render.contains(".write()"),
        "渲染体不得写状态（TUI-RENDER-001）"
    );
    assert!(!render.contains("tokio::spawn"), "渲染体不得发起副作用");
}

#[test]
fn test_tasks_enter_still_cancels_not_open_detail() {
    let main_src = panel_source();
    assert!(
        main_src.contains("cancel_bg_task") || main_src.contains("kill_workflow_run"),
        "Tasks Enter 应取消 bg 任务"
    );
    assert!(
        !main_src.contains("open_panel"),
        "Tasks 面板 Enter 不得打开 SubAgent/Shell 详情抽屉"
    );
}

/// P0-74：底部导航提示必须描述真实动作——后台任务行 Enter 取消，Cron / SubAgent 行 Enter 只关闭。
#[test]
fn test_tasks_nav_hint_describes_cancel_not_open() {
    let main_src = panel_source();
    assert!(
        !main_src.contains("common-nav-enter-close"),
        "Tasks 面板不得复用 Enter::open 语义的通用提示"
    );

    // 动作侧：Enter 从当帧行表解析选中行的身份（旧实现按键帧重读 atom 按下标取
    // task_id，渲染与按键之间列表刷新就会取消另一个任务）
    let enter_arm = main_src
        .split("KeyCode::Enter =>")
        .nth(1)
        .and_then(|rest| rest.split("KeyCode::Up =>").next())
        .expect("Tasks Enter 分支存在");
    assert!(
        enter_arm.contains("activate_row"),
        "Tasks Enter 必须走当帧行表的行动作解析：{enter_arm}"
    );
    assert!(
        !enter_arm.contains("BG_TASKS.state()"),
        "Tasks Enter 不得在按键帧重读列表按下标取身份：{enter_arm}"
    );
    assert!(
        main_src.contains("cancel_bg_task"),
        "Tasks Enter 的真实动作是仅对后台任务行取消；动作语义变化须同步提示与该用例"
    );

    // 取消只可能来自后台任务段的行（段边界取自当帧行表）
    let viewport_src = include_str!("../viewport.rs");
    assert!(
        viewport_src.contains("index >= self.counts.background"),
        "取消目标必须以当帧后台段边界为界：Cron / SubAgent 行不得解析出取消目标"
    );

    // 提示侧：if / else 两支分别选取消 key 与关闭 key——判据与动作一致
    let (then_branch, else_tail) = main_src
        .split("let nav_hint = if sel < bg_count {")
        .nth(1)
        .and_then(|rest| rest.split_once("} else {"))
        .expect("Tasks 提示选择分支存在");
    let else_branch = else_tail
        .split("};")
        .next()
        .expect("提示 if/else 表达式结束");
    assert!(
        then_branch.contains(r#"i18n::tr("panel-tasks-nav-hint")"#),
        "后台任务行提示须使用 Tasks 取消专属 key：{then_branch}"
    );
    assert!(
        else_branch.contains(r#"i18n::tr("panel-tasks-nav-hint-close")"#),
        "Cron / SubAgent 行提示须使用关闭专属 key：{else_branch}"
    );

    for lang in ["en", "zh-CN"] {
        let reg = crate::i18n::LcRegistry::new(Some(lang));
        let cancel = reg.tr("panel-tasks-nav-hint");
        assert!(
            !cancel.contains("Enter::open"),
            "{lang} 提示不得把取消伪装成打开：{cancel}"
        );
        assert!(
            cancel.contains("Enter::cancel") || cancel.contains("Enter::取消"),
            "{lang} 提示须标明 Enter 取消：{cancel}"
        );

        let close = reg.tr("panel-tasks-nav-hint-close");
        assert!(
            !close.contains("Enter::open"),
            "{lang} 提示不得把关闭伪装成打开：{close}"
        );
        assert!(
            close.contains("Enter::close") || close.contains("Enter::关闭"),
            "{lang} 非后台任务行提示须标明 Enter 关闭：{close}"
        );
        assert!(
            !close.contains("Enter::cancel") && !close.contains("Enter::取消"),
            "{lang} 非后台任务行 Enter 不取消，提示不得声称取消：{close}"
        );
    }
}
