# Thread MQ 统一持久收件箱实施

**状态**：实施中。
**类型**：架构重构。
**创建日期**：2026-11-01。
**设计权威**：[Thread MQ](../docs/design/thread-mq.md)（已批准目标设计）。
**基线**：`origin/main` `d0e096d01`。

## 目标

把 `MessageQueue`、`SessionInbox`、`InboxHandle`、`UserInputMailbox` 与 `AsyncResultMailbox` 的待发事实收敛为持久 `thread_mq`：

1. 每个 thread 的 MQ 与 `messages` 位于同一 Thread Store。
2. 用户输入、异步回调与内部通知使用同一入队接口。
3. Receive 在同一事务中写入 `messages` 并删除 MQ 行，提交后才发布事件与镜像 Transcript。
4. 业务方不能获得裸队列、`drain_all`、`drain_batch`、`clear` 或任意删除入口。
5. 展示、模型可见性与唤醒分别判定；Info/UI-only 不自行启动模型。
6. 不恢复旧执行、旧发送命令或外部副作用。

## 已确认决策

1. 用户批准新增 `thread_mq` 表，`CURRENT_SCHEMA_VERSION` 12 → 13；本机迁移、远端 schema 升级与形状测试同步。
2. 新设计取代 `user-input-queue.md` 中「持久 Inbox 已撤销」的旧表述；执行恢复仍不在本主题范围。
3. 业务接口位于 `peri-agent::session`，共享 DTO/策略复用 `peri-acp-types`；存储操作经 `SessionResources` 行为门面进入同一 Store。
4. 本地 SQLite 与远端托管事务批次都实现原子移交；不支持原子能力时显式返回错误，不回退两次独立写入。
5. 保留既有 ACP 方法名、DTO 形状、`generation`/`revision`/`terminalItems`、命令幂等和事件顺序。

## 实施范围

1. **契约与存储**：`thread_mq` schema、本机 v13 迁移、远端 contract 升级、`SessionResources` MQ 原子方法、本地/远端 adapter、删除/rewind 清理。
2. **领域接口**：`ThreadMq` trait 与 DTO；`enqueue/query/cancel_input/send_inputs/receive/wait_for_work`；稳定 ID、排序、未知提交与冲突语义。
3. **运行时**：Receive 原子移交、Transcript 镜像、生产者（UserInputMailbox、AsyncRouter、TaskTerminalDelivery、cron/goal/middleware/child close）收敛。
4. **ACP/TUI**：四个 `session/input/*` 方法改由 Thread MQ 支撑；快照、事件与 generation 兼容行为保持不变。
5. **文档**：设计索引、冲突设计修订、代码索引与 schema 描述同步。

## 验收标准

按 [设计 §8](../docs/design/thread-mq.md) 全部场景：

1. 入队后重开数据库，内容、时间、原稿、附件、目标 thread 与策略一致。
2. 单发越过未选中项；全发固定点击时集合；空闲按队首逐条。
3. 时间相同用稳定消息 ID 排序；不新增序号或改写入队时间。
4. 忙碌普通输入留在 MQ，不打断模型，不被下一次 Receive 无条件排空。
5. 回调接纳失败不提前确认；本地终态与 MQ 接纳共同提交/回滚。
6. 取消与 Receive 竞争只有一个结果；不能删除未入历史消息后报告已发送。
7. Receive 提交前后进程退出的两种事实与重开结果一致，无半移交。
8. 停驻 child 关闭不调用模型；事务失败保留 MQ、不误报 Closed。
9. UI-only 不进入模型；Passive 不自行唤醒；旧 run 控制不影响新 run。
10. ACP/TUI 既有门禁、顺序、幂等与未知回执行为保持。
11. 目标测试、format、`check-changed.sh`、`git diff --check` 通过；未覆盖项明确记录。

## 进度

- [x] 设计文档入库与索引/冲突文档修订
- [x] schema、迁移与存储原子方法（本地 v13 与远端同一 DDL、同一原子批）
- [x] ThreadMq 领域接口与契约测试（`ThreadMqStore`、`Mq*` DTO、内存实现、契约 v7）
- [x] 运行时生产者/Receive/child close 收敛（Receive 同事务移交并镜像 Transcript）
- [x] ACP/TUI 接线（既有 `session/input/*`、快照 DTO 与 generation 不变）
- [ ] 全量目标验证、PR、CI 与 CodeRabbit

## 验证记录

2026-11-02，worktree `.worktrees/thread-mq`（基线 `d0e096d01`），工作树未提交：

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib` | 632 passed / 0 failed / 34 ignored |
| `... -p peri-acp-types --lib` | 697 passed / 0 failed |
| `... -p peri-acp --lib` | 1111 passed / 0 failed / 1 ignored |
| `... -p peri-agent --lib` | 1418 passed / 0 failed / 5 ignored |
| `... -p peri-middlewares --lib` | 1841 passed / 0 failed / 2 ignored |
| `... -p peri-mcp-common --lib` / `-p peri-mcp-workspace --lib` | 夹具迁移后通过 |
| `bash scripts/verify-async.sh`（`check --all-targets` + `clippy -D warnings`） | verdict=pass |
| `bash scripts/check-layer-imports.sh` | 22 条规则，违规边 0 |
| `cargo fmt --check` | 通过 |
| `git diff --check` | 通过 |

未覆盖项：`e2e/` 按 `docs/standards/testing.md` 留在 CI 运行；本地未跑 TUI 人工烟测。
