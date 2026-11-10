# P1：已处理 monitor ALERT 的 covered 回执永不回收

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（资源累积 / 长期会话）。
**创建日期**：2026-11-10。
**来源**：4.7.1 复核证据包 + 探针复现。
**确认方式**：cargo 探针在基线 `4ba78d7e` 实测 30k 累积；主 agent 复核代码路径。

## 确认证据

- 探针 `review_monitor_alert_receipts_accumulate_after_coverage`
  （`peri-agent`）通过，输出
  `covered=1000/10000/30000`、`fingerprint_capacity=1,920,000`、`pending=0`
  —— 处理数量与 `covered` 条目数一一增长，30k 时约 1.92MB 指纹字符串。
- 探针 `review_running_monitor_alert_enters_terminal_coverage_receipts`
  通过：Receive 消费 1 条后 `covered_is_empty()==false`，同 ID 重投
  `AlreadyHandedOff`、`pending=0`。
- 探针 `review_monitor_covered_receipt_survives_sqlite_full_compact`
  通过：`archived_reminder=true retained_covered=true
  replay_without_handoff=AlreadyHandedOff newly_pending=0`
  —— Full Compact 不清理 `covered`。
- 累积点：`peri-agent/src/session/async_result_mailbox.rs:583`
  `state.covered.insert(*id, record.fingerprint)`。
- `covered: HashMap<MessageId, String>`（`:188`）只增不减；唯一清空路径是
  测试辅助 `covered_is_empty`（`:491-493`），生产无回收。
- 重投幂等由 MQ 历史 `AlreadyHandedOff` 兜底：
  `peri-resources/src/sessions/sqlite_store/session_data/mq.rs:306`。

## 根因链

1. monitor ALERT 作为 `McpNotification` 终态结果进入 mailbox（观察）。
2. `cover_after_commit` 成功提交后，把 id 与 fingerprint 写入 `covered`
   （观察：`:583`）。
3. `covered` 没有生命周期回收；Full Compact 只处理消息历史，不触碰 mailbox
   热态（观察：探针 3）。
4. 长期运行的 monitor 会话每处理一条 ALERT 永久占用一份指纹字符串；
   30k 条约 1.92MB，且随运行时间单调增长（观察）。
5. 同 ID 重投的幂等仍由 MQ 历史保证，`covered` 对该类结果并非必需
   （观察：探针 2/3 的 `AlreadyHandedOff`）。

## 影响面

- 用户影响：长驻 monitor 会话内存单调增长；无直接功能错误，但属于长期
  不可回收资源。
- 受影响模块：`peri-agent` async result mailbox / Receive。
- 可规避性：重启会话；生产路径无清理手段。

## 修复方案

- 仅对精确的 monitor ALERT 结果（`McpNotification` +
  `source=monitor` + `kind=alert_matched`）在 `cover_after_commit` 成功后
  不写入 `covered`；依靠 MQ 历史 `AlreadyHandedOff` 保证重投幂等。
- 该判断必须使用结构化来源/类型，不得按 id 前缀或文本匹配。
- 明确不做：
  - 不给整个 `covered` 添加通用 LRU/TTL 或容量上限；
  - 不改变其他终态结果（Shell/SubAgent/Workflow 等）的幂等语义；
  - 不改 MQ 去重协议；
  - 不做 schema 变更。
- 回归测试（探针转正向）：
  - 处理 N 条 monitor ALERT 后 `covered` 不增长或受控；
  - 同 ID 重投仍返回 `AlreadyHandedOff` / `Duplicate`，不重复执行；
  - 非 monitor 终态结果仍写入 `covered`，保持现有幂等与冲突检测。

## 验证记录

- 回归测试（`peri-agent`）：`monitor_alert_coverage_does_not_retain_receipts`、
  `non_monitor_results_keep_covered_receipts`、
  `monitor_alert_consumption_does_not_retain_covered_receipt`、
  `monitor_alert_full_compact_releases_covered_reminder_without_receipt` 均通过。
- `peri-agent --lib` 全量：1515 passed，0 failed，5 ignored。
- `./scripts/check-changed.sh` 与 `cargo fmt --check` 通过。
- 主 agent 复核：4 项回归测试实跑通过。
