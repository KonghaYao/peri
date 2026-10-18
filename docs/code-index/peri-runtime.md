# peri-runtime 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10。
> 依据：`docs/standards/architecture-contracts.md`、`src/runtime.rs` 与契约测试。

## 架构速览

Runtime 持有 session 登记表，定位并转发 `SessionHandle` 操作，为事件补打 session
身份和序列。业务状态及取消裁决归句柄实现；Runtime 不持有持久态。
crate 仅依赖 `peri-acp-types` 的跨层契约，不依赖具体 Agent 或上层装配。

每次登记有独立的 `SessionEntry`，持有句柄与销毁串行化状态。正常句柄刷新共享
`EventClock`，保留 epoch/seq；删除后重新登记使用初始值，跨销毁 epoch 恢复
仍需外部 owner 提供，不能把当前注册表当作持久化纪元来源。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 注册或刷新句柄 | `src/runtime.rs` | `register`、`register_or_replace`、`SessionEntry::new` | register 拒绝重复登记；replace 创建新的登记归属，保留已有事件 clock，即使传入相同句柄 Arc 也独立于旧销毁 |
| 查找会话 | `src/runtime.rs` | `handle`、`contains`、`session_ids` | 读登记表；handle 克隆句柄，外部调用均在表锁之外；列表不保证顺序 |
| 补打事件身份与序列 | `src/runtime.rs` | `stamp`、`EventClock::stamp` | 表读锁内定位当前登记，clock 锁内递增 seq 并复制身份；未登记返回 UnknownSession |
| 转发执行和输入 | `src/runtime.rs` | `run`、`join`、`submit_input` | 查找句柄后转发；run/input 错误保留 anyhow 来源并包装边界错误；join 返回是否如期结束 |
| 转发取消 | `src/runtime.rs` | `cancel` | 原样转发 CancelRequest，最终取消语义与幂等判定归句柄实现（ARC-CANCEL-001） |
| 销毁会话 | `src/runtime.rs` | `destroy` | 捕获登记，按停收、取消、join、超时 abort、persist、drain、移除顺序收尾；只移除捕获的登记，旧事件使用捕获的 clock；同一登记并发销毁等待并仅首次返回事件 |
| 检查销毁失败与取消 | `src/runtime_test.rs` | `destroy_persist_failure_keeps_mapping`、`old_persist_failure_preserves_replacement`、`cancelled_destroy_can_be_retried` | 持久化失败不 drain 或移除，取消 future 释放串行化锁，重试仍可收尾 |
| 检查刷新和旧事件竞态 | `src/runtime_test.rs` | `destroy_preserves_replacement_and_continues_shared_sequence`、`destroy_preserves_a_new_registration_of_the_same_handle`、`late_destroy_does_not_consume_a_fresh_registration_sequence`、`concurrent_destroy_drains_once` | 用可控 join 暂停复现替换、同句柄重新登记、删除后重新登记与并发销毁，无计时睡眠 |
| 修改边界错误 | `src/error.rs` | `RuntimeError` | UnknownSession / SessionAlreadyRegistered / RunFailed / PersistFailed / SubmitFailed |
| 修改共享接口 | `peri-acp-types/src/runtime.rs` | `SessionHandle`、`UnstampedEvent` | 契约事实源；本 crate 从 `src/runtime.rs`、`src/lib.rs` re-export |

## 锁与生命周期

登记表锁不跨 await，也不包围外部句柄调用。每个登记的 Tokio mutex 串行化销毁，
允许 join/persist 等待；取消 future 或 persist 失败会释放锁且不标记完成。
事件 clock 只在同步补打期间加锁，释放后才尝试移除登记，避免锁序倒置。
已等待同一登记的重复销毁返回空事件；完成后才新发起的销毁因映射缺失返回 UnknownSession。

## 跨模块契约

- ARC-CANCEL-001：Controller → Runtime → SessionHandle 定位转发，取消策略和终态归 Agent。
- ARC-EVENT-001：Runtime 补打复用 `peri-acp-types::identity`，Controller 负责返回事件的投递。
- 验证：`cargo test -p peri-runtime --lib`、`cargo test -p peri-controller --lib controller`。
