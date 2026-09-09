# TUI 规则

任务入口为 `peri-tui/CLAUDE.md`；本文件只保留稳定实现约束。

### TUI-HOOK-001

- **Scope**：ratatui-kit `#[component]`。
- **Rule**：所有 `hooks.use_*` 在任何条件分支、`match` 或提前 `return` 前按稳定顺序调用；不得让渲染路径改变 hook 数量或类型。
- **Verify**：`cargo test -p peri-tui --lib`；人工检查组件的所有 hook 位于条件控制流之前。

### TUI-RENDER-001

- **Scope**：组件 render body。
- **Rule**：render body 禁止写 atom；必须在 render 内更新的 `use_state` 派生缓存使用 `write_no_update()`，避免通知触发自激重渲染。副作用写入放在明确的事件或 effect 边界。
- **Verify**：`cargo test -p peri-tui --lib`；人工检查新增 `.write()`、atom setter 与 `use_effect` 的触发条件。

### TUI-THEME-001

- **Scope**：非组件主题读取。
- **Rule**：组件用 hook 订阅主题 atom；非组件按“两步绑定”取得主题值并在 guard 生命周期内完成复制，禁止保存悬垂引用。禁止硬编码颜色。
- **Verify**：`cargo check -p peri-tui`；人工检查主题读取来自 `peri-theme` atoms，且没有新增硬编码色值。

### TUI-EVENT-001

- **Scope**：事件 handler 与交互区域。
- **Rule**：键盘按当前 focus owner 路由；鼠标按最近完成帧的语义命中区域、z-order 与 pointer capture 路由。弹窗和面板等前景区域必须先于背景处理并消费遮罩/关闭事件，避免背景 click-through；消息区的滚动、点击、拖拽与选择必须复用既有语义 handler，不得以注册偶然顺序形成冲突。
- **Verify**：`cargo test -p peri-tui --lib`；人工检查 `use_event_handler` 的 scope、`EventPriority`、`EventResult`、命中区域与 pointer capture 生命周期。

### TUI-I18N-001

- **Scope**：TUI 面向用户的文本。
- **Rule**：只翻译用户可见界面文本；日志、错误、协议、标识符和路径保持原样。新增文本时同步增加 `peri-tui/locales/en/main.ftl` 与 `peri-tui/locales/zh-CN/main.ftl` key，代码用 `i18n::tr`/`tr_args`，需要动态切换的组件订阅 `LANG_VERSION`。
- **Verify**：`cargo test -p peri-tui --lib`；人工核对两份 FTL、调用 key 和语言订阅。

### TUI-TEXT-001

- **Scope**：输入、截断、换行和终端坐标。
- **Rule**：输入与显示文本按 Unicode 字符边界处理；终端列宽用 `unicode_width`；`u16` 几何计算做饱和或显式边界处理，不以 `.len()` 代表显示宽度。
- **Verify**：`cargo test -p peri-tui --lib`；人工检查新增文本测量、截断和坐标运算。
