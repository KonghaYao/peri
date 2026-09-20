# P2：`cargo test -p peri-tui --lib` 全量并行时偶发失败（全局 atom 快照/恢复与未序列化写入方竞争）

**状态**：Open
**优先级**：P2（测试基建，不影响运行时行为，但削弱验证可信度：全量门禁偶发红灯容易被误判成本次改动回归）
**类型**：测试隔离 / flake
**创建日期**：2026-09-20
**来源**：2026-09-19「执行所有权不可得时只读进入」修复集的验证过程（第三路独立 review 的 5 次全量运行 + 主 agent 复跑 5 次）
**最后核查**：2026-09-20（macOS 26.5.1 arm64，本机）

## 问题描述

全量并行运行 `cargo test -p peri-tui --lib`（1658 例）时偶发失败，**每次命中的用例都不同**，单跑与定向重跑全绿。失败断言集中在「全局 atom 应当已经呈现某状态」，因此门禁结果不稳定。

## 证据

2026-09-20 连续 5 次全量运行：**2 次失败**，命中用例分别是

| 用例 | 失败断言 |
| --- | --- |
| `kit::acp_bridge::tests::test_hitl_bridge_drops_unowned_events_before_all_side_effects` | `VIEW_MODELS` 中找不到预期的 `TuiAskUserBlock` |
| `kit::acp_bridge::tests::test_bridge_reset_rehydrates_pending_compact_note_for_same_session` | `!ACP_STATE.state().read().is_loading` |
| `kit::steer_consumer::tests::test_slow_session_preparation_is_not_capped_by_receipt_deadline` | 计时/等待类断言 |

补充证据：`cargo test -p peri-tui --lib -- kit::acp_bridge::tests` 单跑 17 passed、`cargo test -p peri-tui --lib -- recovery_tests` 连跑 10 次全绿——失败用例本身与其邻居在隔离运行时是确定的。

## 归因推断（未做对照实验）

- 命中用例的模式是「快照-恢复全局 atom + 断言某事已经发生」，且都带 `#[serial]`。但 `#[serial]` 只与其他 `#[serial]` 用例互斥：`VIEW_MODELS` / `ACP_STATE` 存在大量**未标** `#[serial]` 的写入方，例如 `peri-tui/src/kit/acp_events_test/session_events_test.rs`、`todo_skill_test.rs`、`command_feedback_test.rs`、`turn_interrupted_test.rs` 里的 `*VIEW_MODELS.state().write() = ViewModelsSnapshot::default()`，可直接清空正在被断言的内容；`input_area_test.rs` / `acp_notifier_test.rs` / `submit_consumer_test.rs` 也有写 `ACP_STATE.is_loading` 的用例（这些文件多数已标 `#[serial]`，需逐条甄别）。
- `UiAtomsGuard` 这类「整体快照 + 结束恢复」的守卫（`peri-tui/src/acp_client/client/recovery_test.rs`）在结束时写回开始时读到的值，同样会覆盖并行用例期间的写入。
- 另一类是计时敏感断言（prepare/receipt 超时相关），满载并行时更易踩线。
- 2026-09-19 的只读准入修复**没有新增**这类写入方（新增用例都带 `#[serial]`），但延长了 serial 占用窗口，使既有竞争更容易被观察到。

## 影响

- 全量门禁偶发红灯，需要人工复跑区分 flake 与真实回归；本轮验证中已出现一次这类误判风险。
- 定向测试不受影响，因此单模块验证仍然可信。

## 建议方向

先做对照实验确认主因（例如把 `acp_events_test` 的相关用例补上 `#[serial]` 后连跑 10 次全量，观察失败率是否归零），再择一收敛：

1. 给会写 `VIEW_MODELS` / `ACP_STATE` 的用例统一补 `#[serial]`（面积大、收益直接）；
2. 或把 `acp_events_test` 整体串行化（共享锁），把竞争面收在测试基建层；
3. 把「等待某事发生」的断言改为事件驱动（`test_hitl_bridge_drops_unowned_events_before_all_side_effects` 里已有的 `observed_rx` 模式），减少计时依赖。
