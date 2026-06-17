# Peri 终端光标修复记录——坐标、残影、越界三层递进

> **[Peri Code](https://github.com/konghayao/peri)** — 用 Rust 写的开源 Coding Agent，兼容 Claude Code 生态。<https://github.com/KonghaYao/peri>

浏览器里的 `<textarea>` 不需要关心光标怎么画——渲染引擎自动处理坐标、移动、闪烁和裁剪。TUI 里没有这层封装——光标位置靠 `set_cursor_position` 手动设，终端模拟器决定怎么渲染和裁剪，组件库还可能自己画一个视觉光标。三个参与者各自独立决策，任一失配都会导致光标异常。

终端光标要在 TUI 输入框里始终可见，需要三个独立条件同时满足——坐标和 textarea 的 scroll 偏移对齐、帧间无残留渲染、计算出的位置不超出视口边界。任何一个条件不成立，光标就会出现消失、残影、或按键不动的症状。三个根因互相独立，但修复顺序决定了暴露顺序——第一层修完后，第二层的症状才显现。

## 水平滚动偏移无法从外部反推

textarea 内部的水平滚动偏移决定光标在视口中的显示列。tui-textarea-2 使用 sticky scroll 策略——光标在可见区域内移动时，scroll 偏移保持不变，只有光标超出视口边界时才调整。但 Viewport 的水平偏移字段对外是私有的，外部只能从光标位置反推。

原反推公式假设光标永远在视口最右列，代入消元后 `visible_col` 恒等于 `visible_width - 1`，与 sticky scroll 的实际行为不一致。光标在长行内部移动时，显示的 terminal cursor 坐标不跟随移动，始终卡在视口最右格。部分终端模拟器在最右列会裁剪或隐藏光标渲染，光标表现为完全消失。

修复方式——vendor tui-textarea-2，暴露 `pub fn scroll_top()` 读取真实 scroll 偏移，替换反推公式。不用外部维护 sticky 状态方案的原因——需要完全重现 textarea 的内部逻辑才能与 Viewport 保持一致，加一个 getter 的改动范围最小。

## CJK 行尾需要 REVERSED，但暴露了组件库的帧间残留

水平滚动修好后光标位置正确，但 CJK（中日韩统一表意文字，每个字符占 2 个显示列，部分终端模拟器在宽字符上裁剪或隐藏光标块/下划线，行尾尤其容易丢失视觉反馈）字符不显示反色。原因是 textarea 内部的 `cursor_style` 被设成了透明——此前为防止与终端光标冲突。恢复 REVERSED（反色显示，交换前景色和背景色）样式为 CJK 宽字符提供视觉兜底。

REVERSED 恢复后，tui-textarea 的行尾光标机制同时产生新帧残留——换行或删除后，前一行行尾出现反色光标块。来源是 `LineHighlighter::into_spans()`——光标位于行尾时，该方法在行文本后追加一个 REVERSED 空格作为组件层视觉光标。下一帧光标移动后，旧位置的 REVERSED 空格可能未被 ratatui（Rust 终端界面渲染框架，Peri 的 TUI 层基于此构建）的帧间 diff 可靠清除。

两套光标并存——textarea 的 REVERSED 空格（组件层视觉光标）和 `Frame::set_cursor_position` 设置的终端硬件光标（IME 定位锚）。两套光标各自独立更新，帧间不完全同步。修复方案是移除 textarea 的行尾光标渲染，光标可视化统一由终端硬件光标负责。

## 坐标越界时终端直接不挪光标

`textarea_cursor_pos` 计算的 `visible_col` 没有任何钳位。当光标位于 scroll 后的视口边缘时，计算出的显示列可能等于或大于 `visible_width`，终端光标坐标超出 textarea 的 inner 区域。

部分终端模拟器在收到超出边界的 Goto（ANSI 转义序列中的光标定位命令）指令时直接忽略，光标原地不动——退格后文本被删除但光标停留在删除前的位置。加一行 `.min(visible_width.saturating_sub(1))` 钳位到视口范围内解决。

三层条件全部满足后，终端光标行为确定——位置跟随 scroll 偏移、无帧间残留、坐标始终在视口内。最初连按 24 次 ← 坐标才动一格的症状，恰好暴露了三层独立根因——光标消失时，单看表现无法区分是坐标算错、残影覆盖、还是终端忽略指令。

项目地址: [github.com/konghayao/peri](https://github.com/KonghaYao/peri)
