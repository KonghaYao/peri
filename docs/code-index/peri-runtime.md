# peri-runtime 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：docs/standards/architecture-contracts.md（ARC-CANCEL-001 / ARC-EVENT-001）、源码（无 crate 级 CLAUDE.md，架构速览取自 lib.rs / runtime.rs 模块注释）

## 架构速览

- 职责：多 session 编排器（docs/design/architecture.md §3）——薄编排：唯一持有 `session_id -> SessionHandle` 映射；无状态、无持久态、无业务配置（session 状态在 Agent 层各 session 内，其余全部注入）
- 数据流：Controller（定位转发入口）→ Runtime 查映射 → `SessionHandle` 方法转发（run/cancel/join/submit_input/destroy）；Agent 层未补打事件 → `Runtime::stamp` 补打（session_id 按 session 维度 + session_seq 单调递增）→ canonical envelope
- 稳定不变量：cancel 只定位与转发、不解释取消语义，幂等判定归 Agent（ARC-CANCEL-001）；`SessionHandle`/`UnstampedEvent` 接口契约事实源为 `peri-acp-types::runtime`，本层仅 re-export（改签名须改事实源）；销毁七步顺序固定（§9 契约，持久化失败映射保留）
- 错误模型：边界类型化 `RuntimeError`（5 个变体），Agent 侧细节错误 anyhow 穿透后逐层包 context（§9 错误模型：边界类型化，层内 anyhow）
- 依赖方向（§0）：仅 `peri-acp-types` / `peri-agent`；不依赖 peri-acp / peri-tui / peri-controller

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 句柄注册 / 替换 | `src/runtime.rs` | `register`（:61）、`register_or_replace`（:93） | register 防双注册（同 session 已存在报 `SessionAlreadyRegistered`，重建须先 destroy）；register_or_replace 已注册则替换句柄且**不递增 epoch / 不重置 seq**（同一 session 新一轮执行事件序号继续单调），未注册等价首次注册 |
| 查映射（handle 定位） | `src/runtime.rs` | `handle`（:125）、`contains`（:120）、`session_ids`（:115） | HashMap 簿记；session_ids 返回 key 列表，无顺序保证 |
| cancel 转发 | `src/runtime.rs` | `cancel(&CancelRequest)`（:169） | 查映射拿句柄（未注册 → `UnknownSession`）→ 原样透传 `CancelRequest`（三元组 identity + clear_queue + policy）给 `SessionHandle::cancel`；重复转发同一请求幂等（句柄侧判定），本层不解释取消语义（契约 ARC-CANCEL-001） |
| 事件聚合补打 | `src/runtime.rs` | `stamp(session_id, &UnstampedEvent)`（:141） | 未注册 session 报 `UnknownSession`（销毁后迟到事件无法补打，与 epoch 不可复用契约共同防迟到消息命中新 session）；已注册则补 session_id、seq 单调递增（`SessionSeq::next`，绝不回退）、epoch 透传当前实例纪元 |
| run / join / submit_input 转发 | `src/runtime.rs` | `run`（:178）、`join`（:195）、`submit_input`（:206） | 查映射 → 转发句柄对应方法；未注册均报 `UnknownSession`；run 错误包 `RunFailed`、submit_input 错误包 `SubmitFailed`（Agent 侧细节 anyhow 穿透）；join 返回 deadline 内是否结束（true/false） |
| destroy 七步编排 | `src/runtime.rs` | `destroy(session_id, join_deadline)`（:226） | 顺序固定：停收新输入 → 取消 owned tasks → join（带 deadline）→ 超时 abort → 持久化事务收束 → drain 事件（逐条 `stamp` 补打）→ 移除映射；持久化失败上抛 `PersistFailed` 且**不移除映射**（已执行阶段幂等，重试安全）；drain 出的补打事件作为返回值交调用方（Controller）投递 |
| 改句柄接口（SessionHandle） | `peri-acp-types/src/runtime.rs`（事实源）；本层 re-export（runtime.rs:21） | trait `SessionHandle`（runtime.rs:77：run/cancel/submit_input/stop_accepting/cancel_owned/join/abort/persist/drain）；`UnstampedEvent`（runtime.rs:29） | 各层接口引用同一签名，本层 `pub use peri_acp_types::runtime::{SessionHandle, UnstampedEvent}` 透传；改接口只改事实源，勿在本层定义 |
| 改边界错误类型 | `src/error.rs` | `RuntimeError`（:8） | 仅对边界可判定条件类型化（UnknownSession / SessionAlreadyRegistered / RunFailed / PersistFailed / SubmitFailed）；Agent 侧句柄实现细节错误经 anyhow 穿透，在本边界逐层包 context |

## 子系统

### 编排器（src/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 多 session 编排 | runtime.rs | `Runtime`（:43）、`new`（:49）、`Default`（:257） |
| 映射条目（句柄 + epoch/seq 簿记） | runtime.rs | `SessionEntry`（:29；seq/epoch 是事件聚合补打的簿记，非 session 业务状态——session 业务状态仍在 Agent 层） |
| 契约类型 re-export | runtime.rs | `pub use peri_acp_types::runtime::{SessionHandle, UnstampedEvent}`（:21） |
| 边界错误 | error.rs | `RuntimeError`（:8：UnknownSession / SessionAlreadyRegistered / RunFailed / PersistFailed / SubmitFailed） |
| 句柄销毁辅助（destroy 编排调用） | `peri-acp-types/src/runtime.rs`（事实源） | `SessionHandle::stop_accepting`（:93）/ `cancel_owned`（:95）/ `join`（:100）/ `abort`（:102）/ `persist`（:104）/ `drain`（:106） | destroy 按七步顺序调用（runtime.rs:235-:252）；句柄实现方为 ACP 层执行薄壳（过渡）或 Agent 层 session 工厂（L5） |
| crate 出口 | lib.rs | re-export `Runtime / SessionHandle / UnstampedEvent`（:20） |
| 测试（MockHandle） | runtime_test.rs | `MockHandle`（:21）实现 `SessionHandle` 并记录调用序列（`call_sequence` :47 / `last_cancel` :51 / `cancel_count` :59） |
| 测试入口（行为契约验证） | runtime_test.rs | `stamp_fills_session_id_and_seq_monotonic`（:168）、`destroy_follows_order_and_drains_stamped_events`（:204）、`destroy_aborts_on_join_timeout`（:232）、`destroy_persist_failure_keeps_mapping`（:255）、`cancel_idempotent_repeated_forward_same_request`（:327）、`cancel_passes_clear_queue_and_policy`（:359） | 覆盖 ARC-CANCEL-001 的 Runtime 侧要求：cancel 转发幂等一致、clear_queue/policy 透传；销毁顺序与补打事件由 `destroy` 验证 |

## 实现要点（边界语义）

- **epoch/seq 簿记**：`SessionEntry` 持 `SessionEpoch`/`SessionSeq`（`peri-acp-types::identity`：`SessionEpoch::initial` :65 / `next` :70，`SessionSeq::initial` :178 / `next` :183）；`register` 新建条目均为 initial，`register_or_replace` 替换句柄时**不动** epoch/seq——事件序号是 session 实例维度而非执行轮次维度
- **UnknownSession 是共同路径**：run（:178）/ cancel（:169）/ destroy（:226）/ stamp（:141）/ submit_input（:206）查映射失败均报 `RuntimeError::UnknownSession`；销毁后迟到事件无法补打（stamp 拒绝），配合 epoch 不可复用契约防止迟到消息命中新 session 实例
- **destroy 幂等**：七步中已执行阶段可安全重跑——持久化失败（步骤 5）上抛 `PersistFailed` 且映射保留，调用方可重试 `destroy`；成功路径最后一步才移除映射（:252），drain 事件经 `stamp` 补打后返回调用方投递（:247-:251）
- **cancel 转发语义**：`handle.cancel(request)` 为同步调用（runtime.rs:173），转发成功即 `Ok`；重复转发同一 `CancelRequest` 幂等（runtime_test.rs:327 验证），clear_queue/policy 原样透传（runtime_test.rs:359 验证），本层不做任何语义解释
- **句柄取用与锁**：`handle()` 每次返回 `Arc::clone` 的句柄副本（runtime.rs:125-:130），转发调用在锁外执行——簿记与句柄调用解耦；`stamp` 例外：需写回 seq，持有写锁完成补打（runtime.rs:146-:151）
- **session_ids 顺序**：HashMap key 列表，无顺序保证（controller_test.rs / controller.rs:348 均注明）；list_sessions 需要的标题/时间元数据经 Controller 存储通道合并
- **生产接线状态**：Agent EventBus → `Runtime::stamp` 的接线随 executor 拆分（L5）落地，本 crate 不承载业务装配；当前 Runtime 由 `Controller::new` 缺省创建空实例（controller.rs:203），部署装配点经 `Controller::with_runtime` 注入
- **无状态原则边界**：seq/epoch 簿记是事件补打需要的最小状态，不属于业务状态——业务状态（消息队列/transcript/终止判定）全在 Agent 层句柄内，Runtime 无持久态、无跨实例记忆（`register` 的 epoch 重建递增属 L5 持久化恢复路径）
- **测试隔离**：`MockHandle` 以 `call_sequence` / `last_cancel` / `cancel_count` 断言转发行为（runtime_test.rs:47-:59），不依赖真实 Agent 运行时，契约测试与实现解耦
- **cancel 优先级不归本层**：cancel > 续跑 > promote > retry 的优先级判定由 Agent 层执行（ARC-CANCEL-001）；Runtime 转发成功后不等待、不判定结果，`Ok` 仅表示已送达句柄
- **stamp 调用方**：`Controller::publish_event`（controller.rs:436-:449）调用 `runtime.stamp` 补打后投递；未注册 session 时 Controller 侧降级为发射方身份直接投递（不 panic）——stamp 的 `UnknownSession` 在该路径被吞掉属设计行为，非错误路径
- **契约验证命令**：`cargo test -p peri-controller --lib controller` 与 `cargo test -p peri-runtime --lib runtime`（ARC-CANCEL-001 Verify 指定的最小验证集，当前 16 + 10 用例全绿）

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-CANCEL-001：cancel 链路 Controller →(定位转发) Runtime →(查映射) Agent 句柄；`CancelRequest` 事实源 `peri-acp-types::identity`（:262）；Runtime 层转发幂等一致、clear_queue/policy 原样透传，幂等判定与 turn 终态归 Agent 层
- ARC-EVENT-001：事件链路单事实源 Agent 发射 → ACP 映射 → TUI 消费；Runtime `stamp` 是聚合补打环节（session_id 按 session 维度补打 + session_seq 单调递增，复用 `peri-acp-types::identity` 类型）
