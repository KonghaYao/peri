# TUI 鼠标事件多层布局路由冲突——集中式遮挡裁决（MouseRouter）

**状态**：Open
**优先级**：中
**创建日期**：2026-08-01

## 问题描述

TUI 中存在多种层叠布局（弹窗覆盖消息区/输入区/状态栏、面板以抽屉形式覆盖消息区底部），鼠标事件在这些层级之间冲突频发：面板打开时消息区抢走滚轮、弹窗遮挡区域误触背景组件、每个背景组件各自维护"是否被遮挡"的判断且经常漏判。需要一个统一的鼠标事件多层路由方案。

## 现状

ratatui-kit 0.10.2 的 `InputRuntime::dispatch`（`input/mod.rs:168`）为两阶段分发：

1. **Phase 1**：所有 `EventScope::Global` handler，按 `(priority desc, order asc)`，`Consumed` 截断全程
2. **Phase 2**：层内 handler，按 `(z-order desc, priority desc, order asc)`

`Global` handler 不受 `blocks_lower` 截断、永远先于层内 handler。渲染循环为 update → draw → dispatch（`render/tree.rs:66-107`），`pre_component_draw` 回填 area 后 dispatch，即"渲染时登记 → dispatch 时查询"可取当帧数据。

当前实际注册形态（已核实）：

| 组件 | 注册方式 | 手动遮挡检查 |
|------|---------|-------------|
| 消息区 `message_area/scroll.rs:250` | `Global, High` | 仅 `POPUP_KIND`；面板靠 flex 空间过滤隐式让路 |
| 输入区 `input_area.rs:531` | `Global, High` | 仅 `POPUP_KIND` |
| 状态栏 `status_bar.rs:203` | `Global, High` | `POPUP_KIND \|\| ACTIVE_PANEL` |
| `model_quick_switch.rs:201` | `Global, High`（无 hit_test，手动行判断） | 无——第四个手动路由组件 |
| 其余弹窗/面板/mention/slash/setup_wizard | `Current, hit_test: true` | 无（依赖框架 hit_test） |

**关键事实（对抗性审查确认）**：

- **面板与消息区不重叠**：面板是 `SessionColumn` 垂直 flex 的一项（`panel_overlay.rs` `Length(height)`），消息区 `Fill(1)` 被压缩在面板上方，不同行。面板 ScrollView 全部默认 `active: true`（ratatui-kit `scroll_view/mod.rs:66-81`），面板区域滚轮在 Phase 2 被面板 ScrollView 消费——**悬停滚轮在面板场景天然成立**。
- **弹窗不开输入层、不用 Modal**（kit 内零 `use_input_layer` 调用）：弹窗覆盖任何背景时，截断完全靠背景组件手动查 `POPUP_KIND` + 前景 hit_test。弹窗盖面板（如 ask_user Esc 确认弹窗）时 confirm `Current+High` 先于面板 `Current+Normal` 拿到事件——**是 priority 碰巧正确，不是机制保障**。

## 冲突根源

1. **遮挡语义倒置**：背景组件注册 `Global High`，在 Phase 1 永远先于前景（Phase 2）拿到鼠标事件。弹窗（`Positioned` 浮层，覆盖消息区/输入区/状态栏）的遮挡必须靠"被遮挡者自觉让路"——分散在 3-4 处、判定不一致（消息区/输入区漏 `ACTIVE_PANEL`，状态栏查全）。
2. **让路检查分散**：三处背景组件各写各的，新增浮层需改多处；`model_quick_switch` 是第四个手动路由点。
3. **hit_test 用上一帧区域**：`use_input.rs:73` `pre_component_draw` 回填 area，新开浮层第一帧点击穿透（背景手动检查为当帧值，故表现为"点击被丢弃"而非误触背景）。
4. **无手势捕获**：拖拽（文本选择、滚动条 thumb）跨区域被 hit_test 切断；消息区靠 Global 绕过 hit_test 才保住拖选，代价是所有鼠标事件无条件进闭包（曾引发 Drag CPU 飙升 issue）。方案 A 不解决此点，列为后续。

## 已确认决策

| 维度 | 决策 |
|------|------|
| 改动范围 | **方案 A：只动 peri-tui，不改 ratatui-kit**（外部 crates.io 依赖，0.10.2） |
| 滚轮语义 | **悬停语义**：滚轮在面板/弹窗区域滚动该层内容，否则滚动消息区 |
| 面板模态性 | **非模态**：面板是消息流上的抽屉，面板区域鼠标归面板、面板外归消息区，不开 `blocks_lower` |
| 后续演进 | 方案 B（框架级命中链 + 手势捕获 + 当帧 area）已论证，作为 A 的后续，不在本次范围 |

## 方案设计（分两档，对抗性审查后定稿）

### A1：集中让路判定（推荐先做，改动最小）

`kit/mouse_router.rs`（新文件，~60 行）只提供集中遮挡判定，不引入区域表：

```rust
/// 任何前景模态层激活时，背景组件统一让路。
/// 集中定义遮挡集，替代三处分散的 POPUP_KIND/ACTIVE_PANEL 检查。
pub fn is_occluded() -> bool {
    POPUP_KIND.state().read().is_some() || ACTIVE_PANEL.state().read().is_some()
}
```

| 文件 | 改动 |
|------|------|
| `kit/mouse_router.rs` | 新增 `is_occluded()` + 单元测试 |
| `message_area/scroll.rs:250` | `POPUP_KIND` 检查 → `is_occluded()`（**补上 ACTIVE_PANEL**） |
| `input_area.rs:531` | 同上 |
| `status_bar.rs:210-215` | `POPUP_KIND \|\| ACTIVE_PANEL` → `is_occluded()` |
| `model_quick_switch.rs:201` | 手动行判断前加 `is_occluded()`（防其被其他浮层覆盖时误触） |

A1 即可达成已确认的三项决策：

- **悬停滚轮**：面板区域滚轮由面板 ScrollView（Phase 2，active:true）消费，面板外由消息区消费——**现状已天然成立**，A1 只修弹窗场景（弹窗打开时背景全让路，与现状语义一致）
- **非模态面板**：面板与消息区 flex 不重叠，面板外消息区交互不受影响——现状已成立
- **peri-tui 内止血**：零框架改动

### A2：区域表（语义扩展，需产品决策，不在本次范围）

若未来要"弹窗区域外背景仍可交互"（非模态弹窗），才需要区域表：前景组件渲染时把当帧 area 登记进 `FOREGROUNDS`，背景组件改为 `occluded(&mouse)`（坐标命中任一前景区域才让路）。

对抗性审查的硬性要求（A2 实施前必须满足）：

1. **帧协议**：区域表是"最后一次完整 draw 的快照"，不是实时 UI 状态。必须定义 clear → build → publish（双缓冲），dispatch 只读已发布的快照；同一批事件内打开/关闭浮层会产生穿透或幽灵遮挡，需 generation/owner 校验或"结构变化后停止处理后续鼠标事件"。
2. **ZLayer 不参与分发**：`occluded` 只做"命中任一"，Z 值（Popup=3/Panel=2）不影响 Phase 2 的前景间路由。弹窗盖面板时 confirm（`Current+High`）先于面板（`Current+Normal`）只是碰巧正确——A2 若要依赖层级，需让前景组件真正开输入层（`use_input_layer`），这是框架语义的补课，不属纯区域表。
3. **登记边界**：按"交互遮挡边界"登记（弹窗内容矩形），严禁登记全屏父 overlay（会让非模态界面意外变模态）。`model_quick_switch` 不经过 AreaTracker，是特例，需单独处理。
4. **outside-click**：区域表下"弹窗区域外点击"会落到背景（Phase 1 背景先消费），弹窗永远看不到外部点击——弹窗如需 outside-click 关闭，A2 无法满足，需框架级方案。

## 涉及文件

- `peri-tui/src/kit/mouse_router.rs`（新增，A1 仅 `is_occluded`）
- `peri-tui/src/kit/message_area/scroll.rs`
- `peri-tui/src/kit/input_area.rs`
- `peri-tui/src/kit/status_bar.rs`
- `peri-tui/src/kit/popups/model_quick_switch.rs`（A1 补让路；A2 需特例处理）
- 若实施 A2：`kit/panel_mouse.rs`（AreaTracker 扩展）+ 前景组件 ×10

## 边界与风险

1. **一帧穿透窗口**（A1 不变，A2 亦然）：浮层刚打开那一帧，若点击先于渲染注册到达，背景仍可能处理。现状表现为"点击被丢弃"而非误触，一帧窗口可接受；彻底解决需框架级当帧 area 回填。
2. **面板滚轮依赖 ScrollView active:true**：已核实（`scroll_view/mod.rs:66-81` 默认 true，14 个面板均未传 active）。若未来某面板传 `active:false`（如 plugin.rs 已注释不用 ScrollView），该面板区域滚轮会落空——届时面板需自备滚轮 handler。
3. **弹窗上滚轮 = 无人消费**（现状一致，弹窗列表无滚动需求）。
4. **弹窗 + 面板同屏**（ask_user Esc → confirm 盖面板；thread 切换 confirm；ACP 事件随时弹 Hitl/Rewind/OAuth）：A1 下背景组件全让路 ✓；confirm 与面板同为 root 层，confirm `Current+High` 先于面板 `Current+Normal`，依赖 priority 而非机制，A1 不改动此结构、不新增风险。
5. **测试**：`is_occluded` 纯函数可单测；跨层行为（弹窗打开时三背景让路、面板滚轮归面板）需组件级/事件级验证——e2e `tui-tester` 的 mouse 测试或消息区现有 `scroll_test.rs` 模式。
6. `AreaTracker` 是 `use_hook` 持久槽位（A2 的 `foreground()` 构造器只改初始值，不碰 hook 顺序，TUI-HOOK-001 无风险）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-01 | — | Open | agent | 创建；方案 A 设计待对抗性审查后定稿 |
| 2026-08-01 | — | — | agent | 对抗性审查（advisor + explorer）：推翻"面板覆盖消息区"假设（flex 不重叠）、"弹窗开层"假设（零 use_input_layer）；方案拆为 A1（集中让路，本次范围）/ A2（区域表，需产品决策） |
