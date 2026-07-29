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
- render body 不写 atom；render 内派生缓存使用既有无通知写入模式，副作用放在事件或 effect 边界。
- `#[component]` 的 hooks 必须在所有条件分支、`match` 与提前返回前按稳定顺序调用。
- 交互事件按焦点和优先级分发：消息区只处理滚轮，编辑区处理键盘，面板/弹窗的局部取消不得被全局 handler 截断。
- 用户可见文本使用 i18n；新增 key 同步更新 `locales/en/main.ftl` 和 `locales/zh-CN/main.ftl`。主题从 `peri-theme` atoms 获取，不硬编码颜色。
- 文本编辑、截断与坐标按 Unicode 字符边界和终端显示宽度处理；不得用字节长度替代显示宽度。

## 目标命令

从仓库根目录执行：

```bash
cargo run -p peri-tui
cargo run -p peri-tui -- -a
./dev.sh
cargo build -p peri-tui
cargo check -p peri-tui
cargo test -p peri-tui --lib
```

## 按需引用 / Verify

- 稳定 UI 规则：`../docs/standards/tui.md`。
- 跨模块边界、事件与冻结数据：`../docs/standards/architecture-contracts.md`，重点遵守 `ARC-BOUNDARY-001` 与 `ARC-EVENT-001`。
- 修改 ACP 数据流时，核对 `src/kit/acp_notifier.rs`、`src/kit/acp_bridge.rs` 和对应组件；修改用户界面文本时核对两份 FTL。
- 完成后运行相关 `cargo test -p peri-tui --lib`，并运行 `git diff --check`。不得把密钥、token、密码或连接串写入界面、日志、错误或测试 fixture。
