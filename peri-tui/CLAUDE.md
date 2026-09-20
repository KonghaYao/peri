# peri-tui

## Scope

`peri-tui` 是基于 ratatui-kit 的终端客户端。用户交互主路径经 ACP transport；crate 当前仍直接依赖 `peri-agent`、`peri-middlewares` 等 crate 的类型、配置和桥接代码。TUI 不得直接驱动 agent loop，Agent 执行入口保持在 ACP 会话执行路径。

## 数据流/架构

```text
ACP notification → acp_notifier → acp_bridge / BridgeState
                 → VIEW_MODELS + atoms → components
```

用户提交、取消、会话加载及交互响应经 ACP client/transport 发送；通知在 `kit/acp_notifier.rs` 解码并进入 bridge，`kit/acp_bridge.rs` 维护 `BridgeState` 并发布渲染状态。组件只订阅和渲染状态，不能在 render 中驱动 Agent。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 入口、任务启动、ACP client 生命周期 | `src/kit/entry.rs`、`src/acp_client/` |
| ACP notification 解码与状态发布 | `src/kit/acp_notifier.rs`、`src/kit/acp_bridge.rs`、`src/kit/acp_events/` |
| 全局状态与 ViewModel | `src/kit/atoms.rs`、`src/kit/acp_types.rs` |
| 输入、提交、历史、@mention、slash | `src/kit/input_area.rs`、`src/kit/input_history.rs`、`src/kit/submit_consumer.rs` |
| 消息渲染、滚动、选择 | `src/kit/message_area/`、`src/kit/markdown/`、`src/kit/text_selection.rs` |
| 键盘、鼠标、焦点与事件优先级 | `src/kit/event_handlers.rs`、`src/kit/focus_router.rs` |
| 面板、弹窗与确认交互 | `src/kit/panels/`、`src/kit/popups/`、`src/kit/panel_overlay.rs` |
| 国际化与主题 | `src/i18n/`、`locales/`、`peri-theme` atoms |
| 测试 | 与目标模块同目录的 `*_test.rs` 或 `#[cfg(test)]` 模块 |

输入历史持久化由 `src/kit/input_history.rs` 管理，路径为 `~/.peri/input-history.json`；不要另建平行存储。

## 稳定不变量

- ACP 是交互与 Agent 执行的边界；新增请求、通知或终止事件须覆盖 ACP 映射、bridge 和组件消费，终止事件必须离开 loading 状态。
- `BridgeState` 是 ACP 事件到 `VIEW_MODELS` 与 atoms 的状态边界。切换会话或重置时，必须过滤陈旧 session 事件并清理旧会话状态。
- 会话列表经 ACP 的 `peri.sessionWorkspaceV1` scope 查询：默认 Project、可切 Workspace / All；分页未结束时数量标明“已加载/还有更多”，向下键、PageDown 或 End 接近已加载末尾时追加下一页，重复按键合并同页请求。未绑定旧历史继续列出，`v` 通过只读 history RPC 预览且不切换执行会话；`-c` 精确选择启动工作区内当前相对目录，`-r` / 普通恢复先查询保存 binding 或旧会话保存的 cwd，再由 load 接纳旧根。查询或恢复失败不得通过 `ensure_session` 无声新建或把排队输入发给旧会话。执行所有权不可得不是失败：`session/load` 按只读准入进入（`_meta.peri.sessionWorkspaceV1.read_only` → `SESSION_READ_ONLY`，状态栏说明原因），dirty 只读准入同样走确认——接受取回所有权，取消只是保持只读，取回失败也保留首次只读准入（会话不因重试失败被丢弃）。`SESSION_READ_ONLY` 是交互投影：只有交互客户端写入，每次会话边界清空；宿主会在响应 `session/load` 前回放历史，因此每个可能被回放的 load（含 reset 后的重载与重取）之前都要有一次 `project_session_boundary`，否则两次回放叠加在同一个 `committed` 上、消息区整段重复。写入与执行仍由 host 的 `require_owner` 把关，客户端不复制该规则、不另设输入闸门。
- `ACTIVE_EXECUTION_CWD` 仅在 session 初始化提交后发布，驱动路径展示、文件补全和本地导出；启动 cwd 独立保留给新会话。Hooks / Plugin / MCP 面板按 active session ID 查询实际环境，不能持续回写启动快照。
- History 面板使用单行会话列表与固定详情/操作栏；按容器高度计算视口，列表和只读预览各持有独立滚动状态。刷新按 thread ID 保留选择，执行操作使用已选身份，删除确认固定待删 ID，不能用旧索引查新列表决定目标。
- Config / Model / Login / Betas / Theme 的持久配置仍编辑宿主启动时选中的 `ConfigSource`，面板明确标识“宿主配置”和实际保存路径；权限切换（配置行、Shift+Tab、slash）及会话模型选择等运行请求继续按 session ID 路由。整份配置上送不带 session ID；切换会话不重定位宿主配置写入。同配置源会话刷新 provider 连接并失效模型缓存，保留各自的模型/profile 选择和 frozen 数据。
- render body 不写 atom；render 内派生缓存使用既有无通知写入模式，副作用放在事件或 effect 边界。
- `#[component]` 的 hooks 必须在所有条件分支、`match` 与提前返回前按稳定顺序调用。
- 消息区、输入区、状态栏与后台任务栏的绘制区域由 `kit/layout.rs` 的 `CenterBandHook` 收进居中带（§3.1）。带内的换行宽度、命中列与光标列都以带内相对坐标为准，位置 tracker 必须注册在 band hook 之后，否则记录的是未收窄的整幅宽度。滚动条是窗口级 chrome（锚在终端最右列）：`ScrollbarHook` 必须在 band hook **之前**注册并在 `pre_component_draw` 捕获收窄前的矩形，渲染与命中测试共用该矩形。
- 交互事件按 focus owner、语义命中区域、z-order 与 pointer capture 分发；弹窗/面板前景事件和遮罩必须先于背景处理，避免 click-through。
- 用户可见文本使用 i18n；新增 key 同步更新 `locales/en/main.ftl` 和 `locales/zh-CN/main.ftl`。主题从 `peri-theme` atoms 获取，不硬编码颜色。
- 文本编辑、截断与坐标按 Unicode 字符边界和终端显示宽度处理；不得用字节长度替代显示宽度。
- TUI MCP panel 的 `ServiceRegistry` 持有唯一 non-Clone `McpTaskOwner`；初始化 必须经 pool 的 weak spawner 准入。teardown 顺序为 pool begin-close → owner abort/join → pool close，并检查 `McpPoolShutdownReport`；Incomplete 不得记录为已关闭（ARC-HOST-SHUTDOWN-001）。

## 目标命令

从仓库根目录执行：

```bash
cargo run -p peri-tui
./dev.sh
cargo build -p peri-tui
cargo check -p peri-tui
cargo test -p peri-tui --lib
cargo test -p peri-tui --lib -- app::mcp_lifecycle_tests
```

## 按需引用 / Verify

- 稳定 UI 规则：`../docs/standards/tui.md`。
- 跨模块边界、事件与冻结数据：`../docs/standards/architecture-contracts.md`，重点遵守 `ARC-BOUNDARY-001` 与 `ARC-EVENT-001`。
- 修改 ACP 数据流时，核对 `src/kit/acp_notifier.rs`、`src/kit/acp_bridge.rs` 和对应组件；修改用户界面文本时核对两份 FTL。
- 完成后运行相关 `cargo test -p peri-tui --lib`，并运行 `git diff --check`。不得把密钥、token、密码或连接串写入界面、日志、错误或测试 fixture。
