# 消息区鼠标事件高层设计：单击/拖拽手势状态机 + 焦点单一事实源

**状态**：Design ready（待评审）
**关联 issue**：`2026-08-11-tui-click-expand-broken.md`
**参考实现**：grok-build `xai-grok-pager` 消息区域（scrollback）鼠标事件设计
**创建日期**：2026-08-11

## 0. 摘要

issue 中三个缺陷（坐标空间不一致、手抖 Drag 破坏点击、焦点双轨异步收敛）各自修补只能逐个防回归；本设计借鉴 grok-build 的消息区域鼠标架构，用**两个高层不变量**从结构上消除三类问题：

1. **手势状态机**：Down 只记录意图（pending，屏幕坐标 + 一次性冻结的内容定位），Drag 只有越过阈值才升级为拖拽（armed），Up 只看"是否升级"结算——单击判定不再做 Down/Up 坐标比较，坐标空间不一致与手抖破坏点击的 bug 类别整体消失。
2. **焦点单一事实源**：entry 导航模式收敛为一个共享 atom，退出导航在事件边界同步完成——双轨与收敛窗口期消失。

## 1. 问题类别分析（为什么逐个修补不够）

三个缺陷的根因不属于同一层，但共享一个结构性弱点：**点击/拖拽判定依赖事件时点重新换算坐标，状态迁移依赖多个分散状态协作**。

| 缺陷 | 根因类别 | 为什么补丁防不住复发 |
|---|---|---|
| 1. 单击判定坐标空间不一致 | 换算散布在 Down（视觉坐标）与 Up（屏幕坐标）两处 | 每新增一个换算点（网格前缀、滚动偏移、未来布局变化）都可能再次错位；`is_click` 容差测试恒过因为测试与实现同坐标系 |
| 2. 手抖 Drag 使容差失效 | Drag 无条件改状态（`start_drag` 置 `dragging` + 清锚点），与"容差防手抖"的判定逻辑互相矛盾 | `dragging` 标志与锚点是两个 owner，判定链路跨 `scroll.rs` / `text_selection.rs` / `mod.rs` 三处 |
| 3. 点击输入框不回退焦点 | 焦点双轨（局部 `entry_focus` + 共享 `FOCUSED_ENTRY_KEY`），仲裁读局部、清除只写共享，收敛依赖下一帧 effect | 只要仲裁与清除读写的不是同一事实源，任何时序下都有窗口期 |

## 2. 参考设计剖析：grok-build 消息区域鼠标事件

grok-build `xai-grok-pager`（`src/app/mouse.rs` + `src/app/agent_view/selection.rs` + `src/scrollback/text_selection.rs`）的消息区域（scrollback）鼠标事件采用如下架构：

### 2.1 手势状态机：pending → armed → settled

- **Down**（`mouse.rs:743-760`）：只记录意图，不改任何可视状态。`pending_scrollback_click = Some((col, row))`（**屏幕坐标**）；同时对按下点做命中测试（`begin_pending_text_drag_on`，`selection.rs:346`），命中可选中文本则记 `pending_text_drag`（anchor = 命中结果 + Down 坐标快照），未命中文本则留 `deferred_text_press`。
- **Drag**（`selection.rs:520` `handle_scrollback_drag_motion`）：先做**升级判定** `drag_threshold_exceeded`（`text_selection.rs:466`，`dx >= 1 || dy >= 1`）——**未超过阈值：任何状态都不动**（pending 原样保留，`update_text_drag` 返回 false）；超过阈值才 `arm_text_drag` 建立 `drag_selection`。
- **Up**（`mouse.rs:794-992`）：按优先级结算：`drag_selection` 存在 → `finish_text_drag`（复制）；否则取 `pending_scrollback_click` → 用 **Down 记录的坐标**做命中测试 → 单击动作（选中 entry / 双击折叠）。

### 2.2 关键设计决策（借鉴点）

1. **单击 ⇔ 手势从未升级为拖拽**。Up 时不比较 Down/Up 坐标；点击目标坐标就是 Down 锚点。手抖产生的 Drag 事件（未超阈值）不改变任何状态，点击意图天然保留。
2. **全程屏幕坐标**。命中测试基于**上一帧渲染时构建的几何缓存**（`ResolvedSelectionModel`：屏幕行 → entry/range/block 映射，`text_selection.rs:315-428`），事件处理不做视觉坐标换算。
3. **状态迁移在事件边界同步完成**。点击/拖拽/焦点切换都是"Down 记录 + Up 结算"的完整意图，不依赖下一帧异步收敛。
4. **多击计数**（`selection.rs:881` `handle_scrollback_click`）：`last_click: (Instant, idx, count)` 三元组跨事件计数，单击选中、双击折叠、三击滚动到顶。

### 2.3 不借鉴的部分

- **折叠语义**：grok 是单击选中 + 双击折叠；perihelion 产品语义是单击折叠卡切换折叠（issue 描述与键盘 Enter 对齐）。保持单击折叠，不照搬双击。
- **升级阈值**：grok 用 1 格（`dx>=1||dy>=1`）；perihelion 既有 `is_click` 容差（±1 行 / ±2 列）是产品级手抖容忍，保留为升级阈值，只是**判定时机从 Up 前移到 Drag**。
- **双 pane / block 拖拽等**：perihelion 无对应概念，不引入。

## 3. 高层设计

### 3.1 设计不变量

| # | 不变量 | 消灭的缺陷 |
|---|---|---|
| I1 | **单一坐标空间**：鼠标事件处理全程使用屏幕坐标；内容坐标（视觉行/列）只在 Down 时换算一次并随锚点冻结 | 缺陷 1 |
| I2 | **手势状态机**：Up 结算只看"是否已升级为拖拽"，不做 Down/Up 距离比较 | 缺陷 2 |
| I3 | **状态所有权**：pending 锚点与选区状态分离；Drag 未升级对任何状态零副作用 | 缺陷 2 |
| I4 | **焦点单一事实源**：entry 导航模式 = 一个共享 atom；退出导航在事件边界同步完成 | 缺陷 3 |

### 3.2 手势状态机（核心）

取代现 `selection_down_pos`（视觉坐标锚点）+ `text_sel.dragging` 判定 + `mod.rs` Up 时二次换算的组合。

```rust
/// 消息区内一次左键手势的中间状态（取代 selection_down_pos 的语义）。
enum Gesture {
    /// Down 已记录，未升级为拖拽。字段在 Down 时一次性换算并冻结。
    Pending {
        /// 按下点屏幕坐标（唯一参与判定的坐标）。
        screen: (u16, u16),
        /// 按下点内容坐标（视觉行/列）——Down 时换算，Up 结算直接使用。
        visual: (usize, u16),
        /// Down 时命中测试结果：entry header（可折叠行）或 None。
        /// 冻结命中消除 Up 时对 wrap_map 的二次反查。
        entry_hit: Option<(usize, usize)>, // (slot, local_idx)
    },
    /// 已升级为文本拖拽（选区状态归 TextSelection 所有）。
    Armed,
}
```

事件处理（全部在 `scroll.rs` 的消息区内非滚动条列分支）：

| 事件 | 行为 | 副作用 |
|---|---|---|
| **Down** | 记录 `Pending { screen, visual, entry_hit }`（visual = `row − area.y + scroll_y`，`entry_hit` = 此刻用 `entry_click_target` 反查） | 无可视状态改动；`text_sel` 不动 |
| **Drag** | 读 pending：`is_click(pending.screen, drag 屏幕坐标)` → 未超容差：**零副作用** return（节流照常）；超容差：`start_drag(pending.visual)` + `update_drag(当前视觉坐标)`，状态 → `Armed` | 仅升级时触碰 `text_sel` |
| **Up** | 按状态结算：`Armed` → 既有复制逻辑（提取 + clear）；`Pending` → 单击结算（见 3.3）；滚动条列/区域外照旧 | 结算后复位为 `Idle` |
| **遮挡 / Moved** | 保持现有复位逻辑（`scrollbar_drag` / pending / `text_sel.clear()`） | — |

要点：

- **`is_click` 只比较屏幕坐标**：Down 与 Drag 都天然是屏幕坐标，容差判定不再经过视觉换算——缺陷 1 的坐标空间问题在该判定路径上不复存在（`is_click` 签名改为 `(screen_down, screen_drag)`，删除视觉坐标入参）。
- **`dragging` 标志失去仲裁作用**：Up 单击结算不再读取 `text_sel.dragging`（现状 `mod.rs:985-990` 草稿已删，设计层面确认该检查整个移除）；`dragging` 仅作为"已升级"的渲染指示，不再参与单击判定。
- **`start_drag` 不需要清锚点、也不需要幂等**：升级判定前移后，`start_drag` 只被调用一次（升级瞬间），参数即冻结的 `pending.visual`；`selection_down_pos` 语义被 `Gesture` 取代后删除。

### 3.3 单击结算（Up）

`mod.rs` 单击展开 handler（`EventScope::Global, EventPriority::High`）职责收窄为**消费冻结结果**，不做任何换算：

1. 读 `Gesture`：非 `Pending` → `Ignored`（拖拽结算由 scroll.rs Up 分支负责，保持现有分工）。
2. `pending.entry_hit` 为 `Some((slot, 0))` 且 `pending.screen` 在消息区 / 非滚动条列 → 执行现有"设焦点 + 折叠"动作（与键盘 Enter 语义一致）。
3. 命中后 `text_sel.clear()`（取消选区语义，保持现有行为）。

对比现状：Up handler 从"用 Up 坐标换算视觉行 + `is_click` 比较 + `entry_click_target` 反查"三件事，变为"读冻结结果 + 执行动作"一件事。**滚动/网格前缀下的坐标正确性由 Down 时冻结保证，Up 不再有第二次犯错的机会**。

Down/Up 之间内容漂移（流式追加）时的语义：命中以按下时刻为准（与 grok 用 Down 时缓存模型一致，也是标准 UI 语义）。终端协议下同一手势内不会插入滚轮事件，漂移仅来自流式渲染，影响一行内，可接受。

### 3.4 焦点单一事实源（取代双轨 + effect 收敛）

现状：局部 `entry_focus: State<Option<usize>>`（仲裁与渲染读）+ 共享 `FOCUSED_ENTRY_KEY: Atom<Option<FoldKey>>`（外部清除只写它）；点击输入框后局部状态靠 `use_effect` 下一帧收敛（`mod.rs:1230-1249`）——缺陷 3 的窗口期与双轨同源。

设计（推荐 C1）：

- **新共享 atom `FOCUSED_ENTRY: Atom<Option<FocusedEntry>>`**，`FocusedEntry { slot: usize, key: Option<FoldKey> }`——一次写入即完整表达导航事实，取代 `entry_focus` + `FOCUSED_ENTRY_KEY` 两个状态。`key` 在设焦点时由 `fold_key_of(items[slot])` 派生（无折叠能力 entry / `request_id` 缺失的 interaction 的合法 `key: None` 场景原样保留，`slot` 仍表达"焦点在消息区"）。
- **写入口唯一化**：`set_entry_focus(Option<FocusedEntry>)`（或直接写 atom），所有设焦点/退出路径收口：键盘 Alt+↑/↓、Esc、单击 entry、`BRIDGE_RESET_COUNTER` 会话重置路径。
- **仲裁改读共享**：消息区键盘 handler（`mod.rs:1122`）`message_nav_accepts(&key, focused)` 的 `focused` 改为读 `FOCUSED_ENTRY`（事件 handler 读 atom 合法）——`focus_router` 签名可保持（传参）或内部改读。
- **input_area 点击同步退出**：composer Down 命中时 `FOCUSED_ENTRY.state().write() = None`（现有草稿已写 `FOCUSED_ENTRY_KEY`，改为新 atom），**事件边界即完成**，无需 effect 收敛。
- 渲染订阅：render body `use_atom(&FOCUSED_ENTRY)`（取代 `use_atom(&FOCUSED_ENTRY_KEY)` + 局部 `entry_focus` 的读取），selection border 与高亮从同一事实源派生。
- 旧 `FOCUSED_ENTRY_KEY` 迁移：全部读点（`input_area.rs`、`mod.rs`、`focus_router`）随迁移删除；若存量外部依赖（如日志）需要，提供 `FocusedEntry::fold_key()` 访问器而非保留第二事实源。

代价与约束：atom 是全局的，会话切换/重置路径须覆盖（`FOCUSED_ENTRY_KEY` 已有先例，同一处一并重置）；`interaction_option`（局部）保持局部派生——它只跟焦点联动、不参与跨组件仲裁，无需提升。

### 3.5 测试基建：状态机纯函数化 + 事件链用例

issue 遗留指出"组件级 Down→Drag→Up 事件链无自动测试"。设计上分两层解决：

1. **状态机纯函数化**：`Gesture` 的转移逻辑（`on_down` / `on_drag` / `on_up`，含 `is_click` 升级判定）提取为不依赖 hooks 的纯结构（类似 grok 的 `AgentView` 方法直接可测），单测覆盖全部转移表——**手抖不升级、升级后不再回退、Up 结算**等转移在纯层锁定。
2. **事件链场景测试**：`peri-tui` 内建 `kit/mouse_test.rs` 测试助手，构造 `Event::Mouse` 序列（Down→Drag→Up）按事件优先级注入既有 handler 路径，断言端到端结果（展开切换 / 选区复制 / 焦点回退）。核心场景表：

| 场景 | 事件序列 | 断言 |
|---|---|---|
| 滚动/网格前缀下原地单击 | Down → Up（同屏坐标） | 折叠切换；`text_sel` 无选区 |
| 手抖（容差内 Drag） | Down → Drag(+1 列) → Up | 折叠切换（不升级） |
| 真实拖拽 | Down → Drag(+5 行) → Up | 选区复制；不触发折叠 |
| 点击输入框后 Enter | Down(composer) → Up → Enter | `FOCUSED_ENTRY == None`；Enter 提交 |
| 遮挡中途拖拽 | Down → 弹窗 → Up | 状态复位；弹窗关闭后点击正常 |

## 4. 与现有修复草案的取舍

| 草案条目 | 处置 | 理由 |
|---|---|---|
| 1. Up 用视觉坐标构造并与 Down 比较 | **吸收并前移**：换算冻结在 Down，Up 只消费冻结结果 | 消除 Up 二次换算，坐标错位类 bug 无再生点；`is_click` 容差语义保留 |
| 2a. Drag 不再清空锚点 | **被状态机取代**：升级判定前移后锚点在升级瞬间消费，无需保留"给 Up 比较" | 状态所有权更清晰（I3） |
| 2b. 删除 `dragging` 检查 | **保留（确认）** | 与 3.2 一致；`dragging` 降级为渲染指示 |
| 3a. input_area 写 `FOCUSED_ENTRY_KEY = None` | **迁移**：写新 `FOCUSED_ENTRY` | 单一事实源 |
| 3b. effect 收敛局部状态 | **删除** | 仲裁与清除同源后无窗口期（遗留中"一致性检查"防御不再需要；其担心的"合法 `entry_focus=Some` 而 key=None"场景由 `FocusedEntry { slot, key: None }` 显式表达，不再是不一致） |

## 5. 落地步骤（Slices）

- **S1 手势状态机**（`scroll.rs` + `mod.rs` 单击 handler + `text_selection.rs` 签名调整）：引入 `Gesture`、Down 冻结、Drag 升级判定、Up 结算；删除 `selection_down_pos`；`is_click` 改屏幕坐标入参；纯函数状态机 + 单测。→ 验证：既有 `scroll_test::test_is_click_same_screen_pos_with_scroll_offset` 等回归 + 新增转移表测试。
- **S2 焦点单一事实源**（`atoms.rs` + `focus_router.rs` + `mod.rs` + `input_area.rs`）：`FOCUSED_ENTRY` atom、写入口收口、仲裁改读、input_area 同步退出、会话重置路径覆盖、删除 effect 收敛块。→ 验证：`focus_router_test` 更新 + 键盘/鼠标焦点回归。
- **S3 事件链测试基建**：`kit/mouse_test.rs` 助手 + 5 个核心场景用例（含"错误屏幕坐标比较必须判为拖拽意图"的锁定断言，防止坐标空间回退）。
- **S4 收尾**：`FOCUSED_ENTRY_KEY` 读点清理、`dragging` 语义注释更新、issue 状态更新。

## 6. 验证

- `cargo test -p peri-tui --lib`（既有 1073 + S1/S2/S3 新增）；`cargo clippy -p peri-tui --lib -- -D warnings`；`git diff --check`。
- **用户实测清单**：滚动长会话中点折叠卡展开；手抖点击不失效；拖拽复制正常；点击输入框后 Enter 提交；弹窗遮挡后交互复位；Alt+↑/↓ 导航 Enter 切换（iTerm2）。
- 若实测通过，`2026-08-11-tui-click-expand-broken.md` 状态改 Fixed。

## 7. 风险与权衡

- **Down 冻结命中的漂移语义**：流式追加导致 Down/Up 间内容上移一行时，命中以按下时刻为准——与 grok 一致，标准 UI 语义；若未来要求"以松开时刻为准"，仅需把冻结命中改为 Up 时用 `pending.screen` 反查（屏幕坐标仍唯一，不回退缺陷 1）。
- **atom 提升的生命周期**：`FOCUSED_ENTRY` 须覆盖 `BRIDGE_RESET_COUNTER` 重置；漏一处即会话切换后焦点残留——S2 测试覆盖。
- **保持单击折叠产品语义**：不因 grok 是双击折叠而改交互；但多击计数（`last_click`）机制可留作未来双击选词等扩展（不在本设计范围）。
- **键盘 Alt 方向键终端配置问题**（issue 遗留）：不属本设计范围，另议备选键位。

## 8. 参考代码位置

- grok-build：`xai-grok-pager/src/app/mouse.rs`（Down/Up 结算）、`src/app/agent_view/selection.rs`（pending/arm/升级）、`src/scrollback/text_selection.rs`（阈值与命中测试）。
- perihelion 现状：`peri-tui/src/kit/message_area/scroll.rs`（Down/Drag/Up 分支）、`mod.rs`（单击展开 handler、entry 键盘导航、effect 收敛）、`input_area.rs`（composer Down）、`focus_router.rs`（`message_nav_accepts`）、`text_selection.rs`（`start_drag`）。
