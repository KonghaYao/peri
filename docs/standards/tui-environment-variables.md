# TUI 环境变量控制表

本表是 [Peri 环境变量控制表](environment-variables.md)的 TUI 专属部分，记录当前已实现行为。新增、删除或改变 TUI 变量时，按 [ENV-CATALOG-001](environment-variables.md#env-catalog-001) 同步更新；待实施的清理目标见[环境变量使用清理 issue](../../spec/issues/2026-10-01-environment-variable-usage-cleanup.md)。

| 变量 | 控制什么；有效值与缺省行为 | 消费入口 |
| --- | --- | --- |
| `PERI_IMAGE` | 图片协议覆盖：`kitty` 强制 Kitty，`iterm2` 记录 iTerm2 能力（当前渲染仍禁用），`off` 或未知非空值禁用；空值走自动探测。大小写不敏感。 | `peri-tui/src/kit/terminal_caps.rs` |
| `NO_COLOR` | 存在即关闭颜色，即使为空；同时禁用 italic。 | 同上 |
| `TERM` | `dumb` 时关闭颜色并降级 ASCII 字符；其他值按常规能力探测。 | 同上 |
| `COLORTERM` | `truecolor` 或 `24bit` 宣告 24 位颜色；有其他值时关闭 truecolor 判定，未设置时参考终端品牌。 | 同上 |
| `TERM_PROGRAM` | 辅助判断 truecolor、italic 和图片协议；Kitty/Ghostty/WezTerm/Warp 可自动选 Kitty，iTerm.app 记录 iTerm2。 | 同上 |
| `TMUX` | 存在时禁止自动选择图片协议；显式 `PERI_IMAGE` 覆盖优先。 | 同上 |
| `LC_ALL` | 终端 locale 的优先来源；为 `C` 或 `POSIX` 时使用 ASCII 符号。 | 同上 |
| `LANG` | `LC_ALL` 缺失时的 locale；为 `C` 或 `POSIX` 时使用 ASCII 符号。 | 同上 |
| `PERI_SCROLL_THROTTLE_MS` | 消息滚动节流毫秒数；有效整数至少按 1 ms 处理，`TuiConfig.scroll_fps` 优先；缺省 50 ms。 | `peri-tui/src/kit/message_area/scroll/throttle.rs` |
| `PERI_RENDER_TIMING` | `1` 或 `true`（不区分大小写）时记录渲染阶段耗时；缺省关闭。 | `peri-tui/src/kit/message_area/vm_cache.rs` |
| `PERI_NO_HIGHLIGHT` | `1` 或 `true` 时关闭消息区选中文本高亮；缺省关闭此回退。 | `peri-tui/src/kit/message_area/mod.rs` |
| `PERI_DISABLE_DRAG_SELECT` | `1` 或 `true` 时跳过消息区鼠标拖拽选中；缺省允许。 | `peri-tui/src/kit/message_area/scroll/event.rs` |
| `EDITOR` | 编辑记忆文件时启动的编辑器；缺省 `vi`。 | `peri-tui/src/kit/panels/memory.rs` |

`HOME`、`USERPROFILE`、`PATH` 与 `MALLOC_CONF` 等跨组件 OS 输入仍在[主表](environment-variables.md#操作系统与进程输入)说明；它们并非 TUI 专属开关。
