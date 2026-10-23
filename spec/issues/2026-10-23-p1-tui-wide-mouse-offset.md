# P1：宽终端居中带失效导致鼠标命中与拖动选区整体偏移

**状态**：已修复，待 PR 合并。
**优先级**：P1。
**类型**：回归（鼠标交互坐标错误）。
**创建日期**：2026-10-23。
**来源**：用户报告。
**确认方式**：真实 TUI 运行时复现 + explorer 只读确认 + 主 agent 复核（基线 cd18db7aff48ca359bccf9139ab6acb56e116a71）。

## 确认证据

### 运行时复现（真实 TUI，140×40，隔离 HOME + 本地假模型）

运行：`target/debug/peri`（worktree 基线构建），tmux 140 列，发送一条长回复。

1. 渲染坐标：`MOUSE-REPRO-ANCHOR` 位于 0-based 行 5、列 4；行首为 ` │  ` 前缀，消息区实际从屏幕第 0 列开始。`GridSpec::grid_for(140)` 给出 `left_pad=11`、`band_width=117`。
2. Copy 按钮视觉位置：0-based 列 99..102。真实 SGR 鼠标按下+释放于列 100 → 无反馈；列 111（视觉列 + 11）→ 命中，状态栏显示 `1003 chars copied`。
3. 拖动选区：在可见文本 0-based 列 20..41 拖动（屏幕文本为 `ck brown fox jumps ov`），真实复制结果 `01: the quick brown `，即按 9..30 列提取，整体左移 11 列（=left_pad）。
4. Keep Going 按钮：视觉位于 0-based 行 22、列 105；同一偏移模式。

窄终端（80 列）`left_pad=0`，命中与选区正常。

### 代码证据

- `peri-tui/src/kit/layout.rs:26` `center_band_area` 纯函数正确；`:49-68` `CenterBandHook` 在 `pre_component_draw` 中改写 `drawer.area`。
- 依赖 `ratatui-kit 0.12.0`：`src/component/instantiated_component.rs:143-157` 在 `pre_component_draw` 后执行 `drawer.area = area` 重置；`calc_children_areas` 使用重置后的整幅区域。`0.10.3` 同函数没有这次重置，属依赖升级引入的行为变化。
- 因此 hooks 链中 `ScrollbarHook`（`message_area/props.rs:71-74`，先注册）看到整幅，`CenterBandHook`（`message_area/mod.rs:134-137`）收窄，`MsgAreaTracker`（`props.rs:40-45`，后注册）记录 band；实际绘制子节点却使用整幅。
- 坐标消费点均以 band 为基准：
  - 复制按钮命中 `message_area/hits.rs:110,146`；keepgoing `hits.rs:287-308`；图片/interaction `hits.rs:174-257`。
  - 拖动选区列 `message_area/scroll/gesture.rs:26-31`；消息区/外框判定 `scroll/event.rs:276-277,438-439`。
  - 换行宽度 `message_area/mod.rs:157-161,225`。
- 同一模式注册 `CenterBandHook` 的组件：`input_area.rs:139-142`、`status_bar.rs:381-386`、`bg_task_area.rs:57-60`；分别以 band 为坐标基准（`input_area/hooks.rs:62-65`、`status_bar.rs:167-218`、`bg_task_area.rs:62-78`、`bg_task_click.rs:87-88`）。

## 根因链

1. 用户进入 Wide 断点且 `left_pad > 0`。
2. `CenterBandHook` 在 `ratatui-kit 0.12.0` 中只影响后续 hooks 观察到的 `drawer.area`，不影响组件自身绘制与子节点布局（`drawer.area` 被重置）。
3. 渲染按整幅宽度、从 x=0 开始；`MsgAreaTracker` / `ScrollbarHook` / `AreaTracker` 及所有命中、选区公式按 band（x=left_pad）计算。
4. 用户可见后果：点击按钮需向右偏移 `left_pad` 才命中；拖动选区高亮/复制内容相对光标左移 `left_pad`；输入区、状态栏、后台任务栏同样错位；长消息的实际折行宽度与计算的 `vis_width` 不一致。

## 影响面

- 用户影响：宽终端（约 ≥118 列，`left_pad > 0`）下消息区按钮、交互选项、图片行、keepgoing、输入框定位、状态栏/后台任务点击与文本拖动选区全部偏移 `left_pad` 列；复制内容错误。窄终端可规避。
- 受影响模块：`kit/layout.rs`、`kit/message_area/{mod,props,hits,scroll/gesture,scroll/event}.rs`、`kit/input_area.rs`、`kit/status_bar.rs`、`kit/bg_task_area.rs`、`kit/app_shell.rs`。
- 是否可规避：是——使用 <100 列终端。

## 修复方案

- 根因修复：用 ratatui-kit 0.12 支持的布局期收窄替代失效的 hook 改写。新增透明包装组件 `CenterBand`（手动 `impl Component`；`update` 中 `set_transparent_layout(true)` + `update_children`；`draw` 中 `drawer.area = center_band_area(drawer.area, grid)`），参考上游 `Border` / `Positioned` / `ScrollView` 的 draw 期收窄模式。透明布局使父级仍按子组件原有 `Fill/Length` 约束分配区域。
- 调用点收窄：`SessionColumn` 中包裹 `MessageArea` 与 `InputArea`；`AppShell` 中包裹 `StatusBar` 与 `BgTaskArea`。这样组件自身 hooks（`MsgAreaTracker` / `AreaTracker`）观察到的区域就是 band，现有相对坐标公式保持不变。
- 删除四处 `CenterBandHook` 注册与 hook 类型，避免双重收窄与误导。
- 滚动条：`ScrollbarHook` 原依赖“收窄前外框”锚定终端最右列。包装后其 `pre_component_draw` 只能看到 band，需在渲染体写入终端宽度，显式构造 `Rect { x: band.x - left_pad, y: band.y, width: term_w, height: band.height }`（term_w 不可用时回退 band），继续作为滚动条绘制与“留白起拖”外框。
- 明确不做：不修改 Scrollbar 视觉设计、不改拖动/复制业务规则、不重构 PanelOverlay、不调整 GridSpec 断点数值。
- 回归测试：
  - `CenterBand` 渲染级测试：140 列下 `left_pad=11`，断言标记出现在 buffer x=11、x=0 为空；80 列时 x=0；220 列时 x=51。
  - tracker 一致性测试：受测子组件 hook 记录 `drawer.area`，断言等于 buffer 中实际绘制起点（x=left_pad、width=band_width）。
  - 选区坐标测试：用 `center_band_area(Rect::new(0,0,140,40), grid_for(140))` 调 `selection_position(..., (20, y), 0)`，断言内容列 = 20 - 11 = 9。
  - 按钮命中测试：用同一 band 断言 `area.x + btn.x_start` 等于视觉按钮列（可通过渲染 buffer 与 hit 公式对拍）。

## 验收标准

1. 140 列终端下：视觉 Copy / Keep Going 按钮点击即命中；在可见文本拖动选中/复制的内容与光标位置一致；输入框、状态栏、后台任务栏与消息区同宽收进 band。
2. 80 列终端行为不变。
3. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- center_band`、`-- message_area`、`-- status_bar`、`-- input_area` 通过；`./scripts/check-changed.sh` / fmt / clippy build 通过。

## 验证记录

修复后在 140×40 真实 TUI（隔离 HOME + 本地假模型）复验：

- 布局：消息正文从 0-based 列 15 开始（band x=11 + 前缀 4）；输入框与状态栏收进 11..127；Copy 按钮 0-based 列 110。
- 按钮：真实 SGR 点击视觉 Copy 列 111（1-based）→ 状态栏显示 `1003 chars copied`。
- 选区：在可见列 20..41 拖动 → 剪贴板精确为 `01: the quick brown f`（修复前为 `01: the quick brown `，整体左移 11）。
- 单测：`center_band` 7 passed；`message_area` 359 passed；`status_bar` 27 passed；`input_area` 72 passed；`-p peri-tui --lib` 全量 2310 passed / 9 ignored。
- 门禁：`./scripts/check-changed.sh` 通过；`git diff --check` 通过。
