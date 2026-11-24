# TUI 环境变量控制表

本表是 [Peri 环境变量控制表](environment-variables.md)的 TUI 专属部分，记录当前已实现行为。新增、删除或改变 TUI 变量时，按 [ENV-CATALOG-001](environment-variables.md#env-catalog-001) 同步更新。

| 变量 | 控制什么；有效值与缺省行为 | 消费入口 |
| --- | --- | --- |
| `PERI_IMAGE` | 图片协议覆盖：`kitty` 强制 Kitty，`sixel` 强制 Sixel，`iterm2` 记录 iTerm2 能力（当前渲染仍禁用），`off` 或未知非空值禁用；空值走自动探测。大小写不敏感。 | `peri-tui/src/kit/terminal_caps.rs` |
| `NO_COLOR` | 存在即关闭颜色，即使为空；同时禁用 italic。 | 同上 |
| `TERM` | `dumb` 时关闭颜色并降级 ASCII 字符；含 `sixel`、`mlterm` 或 `foot` 时标记 Sixel 能力（实际像素渲染还需终端报告单元格像素尺寸，否则文本降级）；其他值按常规能力探测。 | 同上 |
| `COLORTERM` | `truecolor` 或 `24bit` 宣告 24 位颜色；有其他值时关闭 truecolor 判定，未设置时参考终端品牌。 | 同上 |
| `TERM_PROGRAM` | 辅助判断 truecolor、italic 和图片协议；Kitty/Ghostty/WezTerm/Warp 自动选 Kitty，Windows Terminal（含 `Windows_Terminal` 写法）标记 Sixel 能力，iTerm.app 记录 iTerm2。 | 同上 |
| `WT_SESSION` | Windows Terminal 会话标识；存在时标记 Sixel 能力。该变量不能区分 1.21/1.22，且 Windows 侧无法取得单元格像素尺寸，因此实际仍走文本降级。 | 同上 |
| `TMUX` | 存在时禁止自动选择图片协议；显式 `PERI_IMAGE` 覆盖优先。终端剪贴板通道下额外发送 `\ePtmux;` DCS 透传副本：tmux 原生 OSC 52 需 `set-clipboard on` 且客户端终端 terminfo 带 `Ms`，透传需 `allow-passthrough on`。 | `peri-tui/src/kit/terminal_caps.rs`、`peri-tui/src/kit/clipboard.rs` |
| `ZELLIJ`、`ZELLIJ_SESSION_NAME`、`ZELLIJ_VERSION` | 任一存在时禁止自动选择图片协议（zellij 不保证图形序列留在 pane 内）；显式 `PERI_IMAGE` 覆盖优先。 | `peri-tui/src/kit/terminal_caps.rs` |
| `SSH_CONNECTION`、`SSH_CLIENT`、`SSH_TTY` | 任一存在时，文本复制经 OSC 52 发往客户端终端，不写 SSH 服务端的系统剪贴板；Ctrl+V 读不到本机剪贴板时提示改用终端原生粘贴。 | `peri-tui/src/kit/clipboard.rs`、`peri-tui/src/kit/input_area/paste.rs` |
| `HERDR_ENV` | 存在时，文本复制经 OSC 52 发往终端，由 herdr 转发；Ctrl+V 同 SSH 处理。 | 同上 |
| `PERI_CLIPBOARD` | 复制通道覆盖：`native` 强制系统剪贴板，`osc52` 与 `terminal` 强制终端 OSC 52；其他值（含 `auto`）与未设置等价，走自动探测——SSH/herdr 或非 macOS 的 Unix 上无 `DISPLAY`、`WAYLAND_DISPLAY` 时用终端剪贴板，其余用系统剪贴板。大小写与首尾空白不敏感。 | 同上 |
| `LC_ALL` | 终端 locale 的优先来源；为 `C` 或 `POSIX` 时使用 ASCII 符号。 | 同上 |
| `LANG` | `LC_ALL` 缺失时的 locale；为 `C` 或 `POSIX` 时使用 ASCII 符号。 | 同上 |
| `PERI_SCROLL_THROTTLE_MS` | 鼠标拖拽（滚动条 / 文本选区）节流毫秒数；滚轮滚动已逐事件落地，不受此值影响。有效整数至少按 1 ms 处理，`TuiConfig.scroll_fps` 优先；缺省 50 ms。 | `peri-tui/src/kit/message_area/scroll/throttle.rs` |
| `PERI_RENDER_TIMING` | `1` 或 `true`（不区分大小写）时记录渲染阶段耗时；缺省关闭。 | `peri-tui/src/kit/message_area/vm_cache.rs` |
| `PERI_NO_HIGHLIGHT` | `1` 或 `true` 时关闭消息区选中文本高亮；缺省关闭此回退。 | `peri-tui/src/kit/message_area/mod.rs` |
| `PERI_DISABLE_DRAG_SELECT` | `1` 或 `true` 时跳过消息区鼠标拖拽选中；缺省允许。 | `peri-tui/src/kit/message_area/scroll/event.rs` |
| `PERI_REDUCED_MOTION` | `1`、`true`、`yes` 或 `on`（大小写与首尾空白不敏感）时关闭尾部折叠组标题行的扫光，标题保留为静态 muted 文本；spinner 与工具图标节拍不变。缺省开启动画。 | `peri-tui/src/kit/terminal_caps.rs`、`peri-tui/src/kit/entry.rs` |
| `PERI_MCP_APPS_TUI` | TUI 侧 MCP Apps 开关：仅 `1` / `true` / `yes` / `on`（大小写与首尾空白不敏感）启用；缺省与未知值关闭。同时需要 `PERI_BROWSER_VIEW_URL` 与 `PERI_BROWSER_VIEW_TOKEN` 就绪，否则运行时整体不启动（零轮询、零 ACP、零 HTTP），只输出一行说明缺哪个变量的 warning。 | `peri-tui/src/launch.rs`、`peri-tui/src/kit/apps/{config,runtime}.rs` |
| `PERI_BROWSER_VIEW_URL` | TUI 消费的 view session 控制面基址；仅接受 `http`/`https` 且不含 userinfo，末尾 `/` 去除；缺省、空白或非法按缺项处理。 | `peri-tui/src/kit/apps/config.rs` |
| `PERI_BROWSER_VIEW_TOKEN` | view session API 的 bearer token（secret）：只进 `Authorization` 请求头，不打印、不落盘、不进 URL、不入日志与错误信息；缺省或空白时运行时关闭。 | `peri-tui/src/kit/apps/config.rs`、`peri-tui/src/kit/apps/view_client.rs` |
| `EDITOR` | 编辑记忆文件时启动的编辑器；缺省 `vi`。 | `peri-tui/src/kit/panels/memory.rs` |

`HOME`、`USERPROFILE`、`PATH` 与 `MALLOC_CONF` 等跨组件 OS 输入仍在[主表](environment-variables.md#操作系统与进程输入)说明；它们并非 TUI 专属开关。
