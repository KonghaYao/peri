# History 恢复会话时 scroll_to_bottom 过早，布局未就绪导致滚动位置停在中间

**状态**：Open
**优先级**：中
**创建日期**：2026-07-11
**类型**：Bug

## 问题描述

从 History 面板恢复历史会话后，消息区的自动吸底在第一帧就执行了 `scroll_to_bottom()`，但此时布局数据（`area_rect`）尚未就绪，`total_visual_rows` 使用了回退宽度（`term_w - 4`）计算。回退宽度与实际渲染宽度不同，导致总行数被低估，滚动偏移量偏小。

后续帧布局就绪、`total_visual_rows` 修正为正确值后，不会再二次触发 `scroll_to_bottom()`（邻近距离 guard 阻止了重滚），用户看到的消息区停在**中间偏上**的位置，无法看到最后几条消息。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板选择 session → 按 Enter 恢复 |
| 实际表现 | 消息区滚动位置停在中间偏上，最后几条消息不可见 |
| 期望表现 | 恢复后自动滚动到最底部，能看到完整的最后一条消息 |
| 复现频率 | 100% 必现 |
| 对话长度影响 | 对话越长，偏离越大（长对话偏得更多，短对话偏离较少） |
| 人工修复方式 | 每次恢复后必须手动 Ctrl+End 或滚轮滚到底 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 进行一次包含一定量消息的对话（至少超过一屏）
  2. 通过 `/history` 打开 History 面板
  3. 选择刚才的对话，按 Enter 恢复
  4. 观察消息区——滚动位置停在中间偏上，不是最底部
  5. 必须手动按 Ctrl+End 才能看到最后一条消息
- **环境**：macOS，ratatui-kit 架构

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— `use_effect` 吸底逻辑（路径 B：`prev == 0 && len > 0` 在首帧触发 `scroll_to_bottom`），`total_visual_rows` 计算依赖 `area_rect`（首帧为 `None` 时用回退宽度）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | deepseek-v4-pro | 创建（issue-create skill） |

## 修复记录

### 诊断日志确证的根因（2026-07-11）

实际日志显示，问题分两阶段：

1. **第一批 replay 事件只有 1 条消息**：`prev==0` 路径触发 `scroll_to_bottom()`，此时 `ScrollViewState.size = None` → `offset.y = u16::MAX` → `render_ref` 裁剪为 0（1 条消息不超屏，`content_height=3, viewport=48` → `min(65535, 0)=0`）

2. **`prev==0` 被过早消费**：prev 变为 1，后续大批次（43/140/150 条消息）永远无法再进入 `prev==0` 路径。路径 D 的距离 guard（`distance > vis_height/4`）因距离过大而永久阻止吸底。

关键日志：
```
Frame 1: items_len=1, prev=0 → scroll_to_bottom(size=None) → offset=65535
Frame 2: items_len=43, prev=1 → distance=148 > threshold=12 → skip
Frame 3: items_len=140, prev=43 → distance=556 > threshold=12 → skip
```

### 修复方案（2026-07-11）

`prev==0` 时启动 20 帧强制吸底窗口：
- 每帧 `set_offset(Position::new(0, u16::MAX))`（不依赖 `self.size` 的前一帧渲染值）
- `render_ref` 在 `post_component_draw` 中裁剪到真实底部
- 20 帧（~333ms）覆盖 replay 所有批次
- 窗口计数器加入 effect deps，每次递减触发重渲染保证逐帧推进

### 调试过程中的错误（经验教训）

| # | 错误 | 教训 |
|---|------|------|
| 1 | `git stash` 操作**反复回滚了已应用的修改**，用户测试的是原始代码，agent 以为已修复 | stash push/pop 后必须 `git diff` 验证修改仍然存在 |
| 2 | 初始诊断为 `area_rect` 时序问题，但日志确认根因是 **replay 批次拆分 + prev==0 过早消费** | 不要根据代码静态分析下结论，必须看诊断日志 |
| 3 | 尝试了 4+ 种过度设计方案（`is_loading=true` 触发 loading spinner、post-load 延迟计数器、loading→idle 过渡检测、deferred prev consumption） | 每次修改后应先验证编译通过且改动确实存在于文件中，再让用户测试 |
| 4 | effect deps 不包含递减计数器，导致 effect 不会重新运行 → 计数器卡住不动 | `use_state` 递减要加入 deps 才能逐帧推进 |
| 5 | `*pl.write() = 0` 试图延迟消费 prev==0，但 effect 重跑后 prev==0 再次命中 → 无限循环重置计数器 | 不要在 effect 内伪造 prev 的前值
