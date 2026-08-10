# 滚轮滚动映射不和谐：ghostty/ssh 平滑滚动下生硬跳变，消息区与面板速度差 3 倍

**状态**：Partial
**优先级**：中
**创建日期**：2026-08-04

## 问题描述

在 ghostty、ssh 等环境下滚动消息区/面板时体验不和谐：平滑滚动手感生硬（阶梯跳变、先动后猛跳、停止时不到位/回弹），消息区与面板的滚动速度明显不一致（同一物理滚轮消息区快 3 倍），粘性吸底在用户上滚 1 格后即失跟。期望滚动手感与终端一致、各区域速度统一、吸底语义稳定。

## 症状详情

### 1. 消息区与面板滚轮步长分裂（3:1）

| 区域 | 实现 | 滚轮 1 格 | 节流 |
|------|------|-----------|------|
| 消息区 | 自研 ScrollPos（`scroll.rs:66` `SCROLL_LINES=3`） | 3 视觉行 | 有（50ms/20fps，`scroll_fps` 可配） |
| 面板 ×13 | ratatui-kit ScrollView（`scroll_view/state.rs:177`） | 1 行 | 无 |
| Plugin 面板 / 全部弹窗 | 无滚动体 | 0（滚轮死区） | — |

同一物理滚轮跨区域滚动时速度断层 3 倍；消息区与面板之间、面板与死区之间切换时体感突变。

### 2. ghostty 平滑滚动下生硬跳变

- ghostty 平滑滚动以 50–200Hz 频率发滚轮事件，应用对每个事件无条件全量渲染（ratatui-kit `render/tree.rs:78-107`，无帧率上限），offset 却按 50ms 阶梯跳 → 渲染频率=事件频率、位置锯齿跳、速度随事件率波动。
- ssh/PTY 缓冲把事件打成 burst：表现为「先动 3 行 → 停 → 猛跳」。

### 3. 滚动停止时不到位/回弹

`pending_delta` 只在事件到达时检查 50ms 窗口（`scroll.rs:243-273`），停手后最后一批滚动量滞留不 flush；反向滚动时与旧 pending 抵消 → 位置错位/回弹。ssh burst 场景下尤为明显。

### 4. 粘性吸底边界问题

- 底部判定混入 +2 padding（`mod.rs:380-388` 追加 2 行空白），滚到视觉底部仍差 2 行不恢复跟随；
- `SCROLL_LINES=3` 下上滚 1 格（=3 行）即整轮失跟，远程环境一次误触即踢出跟随态。

## 复现条件

- **复现频率**：ghostty/ssh 平滑滚动下必现；传统终端偶发
- **触发步骤**：
  1. ghostty（或经 ssh 连接）启动 peri，打开长会话
  2. 触控板/滚轮平滑滚动消息区：观察阶梯跳变与速度放大
  3. 滚动到面板区域：观察速度突变（3:1）
  4. 吸底跟随中上滚 1 格后停下：观察是否恢复跟随、是否多出 2 行空白
  5. 滚动停止瞬间反向滚动：观察位置错位/回弹
- **环境**：ghostty、ssh/tmux 慢 PTY；历史同类环境：Raspberry Pi（`2026-07-20-raspberry-pi-scroll-cpu-high`）

## 涉及文件

- `peri-tui/src/kit/message_area/scroll.rs` —— `SCROLL_LINES=3`、50ms 节流、`pending_delta` 累积、`apply_scroll`、`run_auto_follow` 五分支
- `peri-tui/src/kit/message_area/mod.rs` —— sticky 吸底判定 +2 padding（380-388）
- `peri-tui/src/config/tui_config.rs` —— `scroll_fps` 配置仅消息区读取，名不副实
- `peri-tui/src/kit/entry.rs` —— `EnableMouseCapture` 启用（383-391）
- `peri-tui/src/kit/panels/*.rs`（13 个面板）—— ScrollView 1 行/格、无节流
- `peri-tui/src/kit/list_nav.rs` —— 面板选中项跟随语义（上 1/3）
- ratatui-kit 0.10.2（外部 crate）—— `render/tree.rs` 每事件全量渲染；`scroll_view/state.rs` 滚轮处理
- crossterm 0.29（上游）—— 不请求 `?1016h`（SGR-pixels），像素增量被丢弃

## 关联 issue

- `2026-07-14-auto-follow-loses-track-during-streaming`（Open）：流式突发增长失跟；sticky 改造后可能已修复但未闭环，本 issue 第 4 条为 sticky 语义的剩余边界问题
- `2026-08-01-tui-mouse-multi-layer-conflict`（Open）：面板滚轮归属依赖 ScrollView `active:true` 默认值，与面板滚动实现耦合
- `2026-07-05-scroll-performance-lag`（Fixed）、`2026-07-20-raspberry-pi-scroll-cpu-high`（Fixed）：慢 PTY 渲染风暴的同类失败模式

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-04 | — | Open | agent | 创建 |
| 2026-08-04 | Open | Partial | agent | 修复 #1：flush 尾随 + sticky padding；症状 1（步长分裂）/2（渲染风暴）未修 |
| 2026-08-04 | Partial | Partial | agent | 修复 #2：面板滚轮仲裁层统一步长；症状 2（渲染风暴）仍未修 |

## 修复记录

### 修复 #2（2026-08-04）

- **操作人**：agent
- **用户原意**：同一物理滚轮在消息区与面板间滚动速度断层 3 倍，跨区域体感突变
- **修复内容**：
  - 新建 `peri-tui/src/kit/panel_scroll.rs`：面板滚轮仲裁层——`Global+High`（Phase 1）handler 在面板 ScrollView 默认处理（1 行/事件、无节流）之前拦截：
    - 复用消息区同一节流器与配置（`SCROLL_LINES=3` + `scroll_fps`），消除 3:1 步长分裂
    - `register_panel_scroll(s)` 每帧覆盖注册槽位（kind + 命中区域 + 外部 `State<ScrollViewState>`）；`ACTIVE_PANEL` 匹配校验，面板关闭/切换后绝不驱动失效句柄
    - 命中槽位 → 节流累积 + 3 行/格驱动 → `Consumed` 截断框架默认处理；弹窗打开 / 鼠标不在面板区 → 放行
    - `flush_panel_scroll_due` 渲染帧兜底 flush（ratatui-kit 无 tick 时的停手残留落地，与消息区同语义）
  - `peri-tui/src/kit/atoms.rs`：新增 `PANEL_SCROLL_OWNER`（注册表）与 `PANEL_SCROLL_THROTTLE`（节流器）atoms
  - `peri-tui/src/kit/panel_overlay.rs`：挂载仲裁 handler + 帧兜底 flush
  - 13 个面板（15 个 ScrollView 实例）接入：hooks 区 `use_state(ScrollViewState::default)` + `state: Some(sv)` + `use_previous_size()` + `register_panel_scroll(s)`；双栏面板（model/workflow）用 `split_vertical` 按 45/55、40/60 近似分区
  - 测试：`split_vertical` 切分/clamp、`apply_pending_to_view`（顶部 clamp、未渲染 no-op、哨兵回滚）、`mouse_in_area` 边界
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：待验证（cargo build / 736 lib 测试 / clippy `-D warnings` 均通过；需 ghostty/ssh 实际滚动体验）
- **未覆盖（后续项）**：症状 2（每事件全量渲染）根因仍在 ratatui-kit 外部 crate，需上游 PR 或应用侧帧合并；Plugin 面板/弹窗滚轮死区不在本轮范围

### 修复 #1（2026-08-04）

- **操作人**：agent
- **用户原意**：ghostty/ssh 下滚动不和谐——停止不到位/回弹、吸底跟随失灵
- **修复内容**：
  - `peri-tui/src/kit/message_area/scroll.rs`：
    - 节流重构：提取 `flush_scroll_if_due`（事件与渲染帧双驱动 flush）、`apply_pending`、`apply_delta_to_offset`（向下封顶 max_scroll，消除 offset 无限递增漂移）、`is_reverse_direction`（反向滚动时旧方向 pending 立即落地，不再「先累积后抵消」造成回弹）
    - `should_follow_after_user_scroll` 判定扣除 `SCROLL_PADDING` 滚动缓冲——滚到视觉底部（真实内容底）即恢复吸底跟随
  - `peri-tui/src/kit/message_area/mod.rs`：padding 魔法数 2 收敛为 `scroll::SCROLL_PADDING`；渲染 body 每帧兜底调用 `flush_scroll_if_due`（ratatui-kit 无 tick 时的尾随 flush）
  - `peri-tui/src/kit/message_area/scroll_test.rs`：更新 sticky padding 语义测试（3 个）+ 新增反向判定/位置转换测试（2 个）
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：待验证（cargo build / 729 lib 测试 / clippy `-D warnings` 均通过；需 ghostty/ssh 实际体验）
- **未覆盖（后续项）**：症状 1（3:1 步长分裂）需跨 13 面板 + 外部框架设计；症状 2（每事件全量渲染）根因在 ratatui-kit 外部 crate，需上游 PR 或应用侧帧合并；1016 像素滚动需上游 crossterm 协商

---

## 附录：根因候选（2026-08-04 三路研究结论，供修复参考）

来源：`.peri/plans/scroll-wheel-research.md`、`.peri/plans/scroll-mapping-audit.md`、`.peri/plans/scroll-mapping-issue-analysis.md`

1. **渲染无帧率上限 + offset 阶梯跳**（最可疑）：ratatui-kit 渲染循环对每个事件无条件全量 render，offset 按 50ms 阶梯变化 → 渲染频率=事件频率（>100Hz）、位置 20fps 锯齿跳；慢 PTY 上每次 draw 成本放大。修法方向：渲染侧帧合并/事件批处理。
2. **`pending_delta` 无尾随 flush**：节流只在事件到达时检查时间窗，停手后尾部滚动量滞留、反向时抵消。修法方向：flush 改定时器驱动或事件循环尾随 flush。
3. **`SCROLL_LINES=3` 与事件语义错配**：平滑滚动下「1 事件=1 cell 高」，×3 = 物理滚动速度放大 3 倍；同时每事件 3 次原子通知 → 渲染风暴（07-05 已证 4 次 `terminal.draw()`）。
4. **像素增量被上游吞掉**：crossterm 0.29 `EnableMouseCapture` 从不发 `?1016h`，ghostty 平滑滚动像素增量在第 4 参数被丢弃 → 应用永远只能离散跳、无法感知滚动意图。属上游限制，1016 可协商。
5. **sticky +2 padding 与上滚即失跟**：`mod.rs:380-388` padding 使「滚到视觉底部 ≠ 恢复跟随」；3 行/格使一次误触即整轮失跟。
6. **面板无节流**：`scroll_fps` 是唯一节流配置但仅消息区读取，面板在 ghostty/ssh 高频事件下每事件一次 draw，复现 07-05 已修复的卡顿病。
7. **历史模式**：节流帧率改过 4 档（16ms→33ms→scroll_fps 60/30/20→默认 50ms），全部手动逃生舱、无环境自适应；`run_auto_follow` 5 分支零测试覆盖，7 个 proximity 测试是已废弃死代码。（注：原引用文档 `peri-tui-message-pipeline-v2.md` 已合并入 `docs/design/tui-acp-data-flow.md` §10.3，其中滚动节流描述已修正为默认 50ms/20fps、可配置 `scroll_fps` / `PERI_SCROLL_THROTTLE_MS`，不再与实际脱节。）
