# Peri Swarm 事件日志粒度与回放置位（已实施，验收未完成）

> 状态：**已实施（2026-11-26），验收部分完成**。落地提交
> `877a638e2`（契约与 README 同步）、`530c137ea`（合并行落库失败可见、恢复行不归属旧 turn）、
> `56897e6ff`（取消 turn 的完成行落库失败不改判 failed）；合并基线 `6f935edf6`
> （`feat/deploy-compose`）。验收记录见
> [Agents API 兼容与 item 粒度验收](../../docs/verification/peri-swarm-agents-compat-item-granularity-2026-11-26.md)；
> 本文件保留实施进度与**未验证项**，未验证项不构成验收。
>
> 按 `DOC-HISTORY-001`，全部验收完成后再删除本文件。
>
> 裁决：① 接受回放丢失 chunk 级增量；② `thought` 保留为自定义 item，但按 item 合并；
> ③ 回放真源固定为 PostgreSQL，不引入 Redis，actor 内 ring 只做热尾。
>
> 实施结果：turn 终止时按 item 合并 assistant 文本与推理流；`tool_call`/`tool_result`/
> `approval` 仍逐帧落库；新数据的回放按合并行拼装；取消 turn 的完成行落库失败仍保持
> `cancelled`。
>
> 现行契约：[peri-swarm.md §9](../../docs/design/peri-swarm.md#9-已批准目标事件日志粒度与回放置位已实施)
> 与 §4.2；`peri-swarm/README.md`「官方 SDK 差异」「事件流」已同步。
>
> 基线：`origin/main` `99260a79d`；实施与合并发生在 `feat/deploy-compose`，部署改动不在本 issue 范围。
> 证据：本机单机 Compose（2 副本 + PostgreSQL）实测，2026-11-26；来源为实施/部署报告，
> 未在本 issue 重跑。

## 1. 问题

1. `swarm_items` 按 ACP 通知帧逐条落库：一个 turn 的文本/推理 chunk 各占一行。
2. 表没有自动清理：只有客户端显式 `DELETE /v1/agents/sessions/{logicalId}` 会删行，
   没有 TTL、没有保留期配置。
3. 由此产生两个独立争议：**持久化粒度是否过细**，以及**回放数据该放在哪里**
   （PostgreSQL / 进程内存 / Redis）。

## 2. 实测证据

单机部署、2 个会话、4 个 turn：

| kind | 行数 | 行占比 | 载荷字节 | 字节占比 |
| --- | --- | --- | --- | --- |
| `thought` | 2651 | 71% | 272,127 | 70% |
| `message` | 1068 | 29% | 109,572 | 28% |
| `tool_call` | 12 | <1% | 3,614 | 1% |
| `tool_result` | 6 | <1% | 4,124 | 1% |
| 合计 | 3737 | — | 389,437 | — |

- 约 **930 行/turn**；库整体 11 MB，载荷合计 389 kB。
- 其中 `complete=true` 的行 13 条、带 `inputId` 的用户输入 4 条。
  按 item 去重推算，这 4 个 turn 的真实事实约 **35–40 条**。
- 结论：问题不是磁盘，是**行数与写放大**；空间占用可以忽略。

## 3. 现行契约（不可静默降级）

- 带游标订阅先补 `seq > after` 的 backlog，再转实时广播；`afterSeq=0` 回放全部。
- 无游标订阅只收订阅之后的实时帧，不读历史。
- 审批按同一 item 游标折叠 `state=pending|settled`，重连时先结算过期、再回放、最后补当前快照；
  已结算动作不得复活。
- 回放顺序与实时同序，官方帧由同一 item 行推导；`turn` 控制帧只出现在实时段。
- 实时路径不经过数据库：帧经 actor 内 `broadcast` 通道投递；落库与回放只服务于
  冷重连、跨重启/跨副本恢复与幂等判定。
- 官方事件只发 15 种，reasoning 不在其中；`thought` 仅作为网关自定义 item 暴露。
  事件流的官方 `output_text.delta` 由后续 C 改动补充（实时逐 chunk、回放一条全文），
  不改变本节的持久化粒度。

## 4. 拆成三个独立量

| 维度 | 取值 | 说明 |
| --- | --- | --- |
| 持久化粒度 | 帧 / item / turn | 决定行数与回放精细度 |
| 回放介质 | actor 内存 / PostgreSQL / Redis | 决定能跨重启恢复什么 |
| 清理策略 | 无 / 显式删 / TTL | 决定长期增长 |

三者可独立选择；混在一起讨论会把“粒度问题”误当成“介质问题”。

## 5. 方案对比

| 方案 | 粒度 | 介质 | 行数（同两段会话） | 代价 |
| --- | --- | --- | --- | --- |
| A 现状 | 帧 | PG | 3737 | 写放大；回放逐字 |
| B | item | PG | ~35–40 | 回放不再逐字重播 delta |
| C（推荐） | item | PG + actor 内存有界环形缓冲 | ~35–40 | ring 只做短窗口续读，不作为真相 |
| D | item | Redis 作为回放源 | ~35–40 | 见 §5.1 |
| E | 删 `swarm_items`，会话库为唯一真相 | — | 0 | 见 §5.2 |
| F | 帧 | PG + TTL | 3737（滚动） | 只治增长，不治粒度；会删 `sessions.list` 历史 |

### 5.1 拒绝 D：Redis 作为回放源

1. **没有跨副本共享回放的需求**。一个 `agent/<logicalId>` 的 actor 只在一个副本上运行，
   请求由 engine 路由到承载副本；回放不需要第二个进程读同一份流。
2. **要跨重启就退化成弱化版数据库**。回放要跨进程重启存活，Redis 必须开 AOF/RDB、
   定义 fsync 策略、备份与恢复演练、连接凭证与故障域——本质是第二个有状态存储，
   收益低于它带来的运维面与「两份真相」风险。
3. **审批与幂等仍需要持久真相**。`pending`/`settled` 折叠与 `inputId` 收据若放易失介质，
   重启后会复活已结算的审批或丢 turn 终态；这些事实与回放同源，拆到两处会引入不一致。
4. **热路径本来就不查库**。实时投递已走 actor 内存广播；换 Redis 只改「冷重连」，
   不改善实时延迟。
5. **可保留的余地**：若将来实测写吞吐成为瓶颈，Redis 只能做 write-behind 缓冲，
   不能成为回放真源；那是一条独立的容量决策，不是本提案的一部分。

### 5.2 拒绝 E：删表、以 Peri 会话库为唯一真相

四类事实在 `peri_session` 没有落点：assistant 逐帧文本、`tool_call`/`tool_update` 帧、
审批 `pending`/`settled`、会话内单调游标（`(logical_id, seq)`）。同时会话库可被
compact 标记与 rewind 删行，不能承担「已投递位置」；网关也不应直接读会话库 schema
（两库互为对方 schema 会 fail closed）。要补齐这四类事实，最终仍会重建一张 append-only 表。

## 6. 推荐

**B + C**：

1. `ItemLog` 由「每 chunk 一行」改为「每 item 一行」：同一段 assistant 文本/推理的所有
   chunk 合并，turn 终止写一行 `complete`；`tool_call`/`tool_result`/`approval` 保持逐帧。
2. 回放介质不变（PostgreSQL）：跨重启、跨副本、审批折叠与幂等判定都依赖它。
3. actor 内加有界环形缓冲，只用于小窗口续读，减少短重连打到库；不作为真相，溢出仍发 `gap`。
4. 不引入 Redis。
5. 清理仍只有显式 `DELETE`；是否加 TTL 作为独立可选项（默认关闭），不在本次一并裁决。

预期：同样的 4 个 turn 从 3737 行降到 35–40 行量级；官方事件序列、`/items` 分页、
`afterSeq` 游标、审批折叠与崩溃恢复语义不变。

## 7. 裁决记录（2026-11-26）

| # | 问题 | 裁决 | 后果 |
| --- | --- | --- | --- |
| 1 | 回放是否允许丢失 chunk 级增量 | **允许** | 文本/推理按 item 合并；回放只剩 item 粒度 |
| 2 | `thought` 是否仍作为自定义 item 暴露 | **保留，按 item 合并** | 网关扩展面不变，行数降到推理流条数 |
| 3 | 回放介质是否固定为 PG | **固定 PG，不引入 Redis** | ring 只做热尾，溢出发 `gap` |

实施范围限定在写入粒度与回放拼装；不改表结构、不改官方事件序列、不加 TTL。

### 7.1 实施结果（2026-11-26）

| 提交 | 内容 |
| --- | --- |
| `877a638e2` | 本文改为已实施；`docs/design/peri-swarm.md` §4.2 与 `peri-swarm/README.md` 改按合并行描述 |
| `530c137ea` | 完成组按存储批次提交、失败可见；恢复期 `recovered` 行不归属旧 turn；部分失败时已提交批次先广播 |
| `56897e6ff` | 取消 turn 的完成行落库失败不再改判 `failed`；只记录 warn，终态与持久行保持 `cancelled` |

最终行为：

- turn 终止时按 item 合并：同一 assistant 文本流与推理流各写一条 `complete:true` 行；
  实时 chunk 仍逐帧广播、不落库、不带 `seq`。事件流的官方 `output_text.delta` 由后续
  C 改动补充（实时逐 chunk、回放一条全文），持久化粒度不变。
- `tool_call`、`tool_result`、`approval` 仍逐帧落库；审批保持 `pending`/`settled` 两帧。
- 新数据的回放按合并行拼装；历史多行 item 仍按 `seq` 逐行发 `item` 帧，官方投影按
  `itemId` 折叠（§4.2.2）。
- 未持久化的完成组让非取消 turn 以 `turn.failed` 收尾；已提交批次先广播。
- 取消 turn 的完成行落库失败只记录日志，实时终态仍是
  `turn.completed(status="cancelled")`，持久 turn 行为 `cancelled`，会话回到 `idle`。
- 恢复期写入的 `payload.recovered=true` 行只作为自定义 `item` 回放，不投影官方帧、
  不归属前后 turn。

## 8. 契约变更与迁移

- **无 schema 变更**：仍用 `swarm_items` 现有列与主键；只改写入粒度与回放拼装方式。
- **旧数据可读**：历史 chunk 行与新 item 行按 `seq` 单调共存，回放不需重写数据。
- **无版本门禁**：不需要提升状态库 contract 版本；读取路径向后兼容。
- **回退**：恢复原写入路径即可，期间写入的 item 行仍可被旧代码按帧回放。
- 裁决并实施后，已同步更新 `docs/design/peri-swarm.md`「回放与游标」、
  `peri-swarm/README.md` 的「事件类型」「回放」章节与 `docs/verification/` 验收记录。

## 9. 验收矩阵（实施后状态）

| ID | 必须观察的行为 | 证据要求 | 当前状态 |
| --- | --- | --- | --- |
| ILOG-1 | 单 turn item 行数 ≤ item 数 + 常量 | 计数断言，覆盖纯文本、含工具、含审批三类 turn | 用例覆盖；真实单 turn 观察到 3 行（2 `message` + 1 `thought`） |
| ILOG-2 | `afterSeq=0` 回放顺序与实时一致 | 同一 turn 的实时帧与回放帧逐条比对 | 进程内用例覆盖；线上未实测 |
| ILOG-3 | 断线重连按游标补齐，不重复、不丢 turn 终态 | 订阅中断 → 带 `Last-Event-ID` 重连 | 进程内 mid-turn 重放用例覆盖；真实部署 `afterSeq` 断线重连未实测 |
| ILOG-4 | 审批折叠：已结算不复活，过期先结算 | 崩溃注入 + 重连回放 | 进程内用例覆盖；真实崩溃注入未跑 |
| ILOG-5 | 崩溃恢复不重复执行 initial input | `last_seq` 边界用例 | 进程内持久半态用例覆盖；真实进程 kill 未在本轮跑 |
| ILOG-6 | 官方 15 种事件序列不变 | 与基线录制帧比对 | 用例覆盖；真实 turn 观察到 `created → turn.created → turn.in_progress → item.done → turn.completed → idle`；后续 C 追加 `output_text.delta` 后为 16 种，持久化粒度不变 |
| ILOG-7 | 溢出 ring 后仍发 `gap` 并可续读 | 慢消费者用例 | 进程内用例覆盖；线上慢消费者未实测 |

## 9.1 实施后仍未验证（2026-11-26）

- **历史多行 item 的真实 SSE 回放未实测**：只在既有数据上核对过一条 10 行 thought 的
  `/items` 折叠结果；没有含 ≥2 个 complete 帧的条目，无法复现两代 complete 的历史形状。
- **`afterSeq` 断线重连未在真实部署中断线重测**：目前只有进程内 mid-turn 重放用例。
- **审批折叠未在真实宿主或崩溃注入下实测**：`pending`/`settled` 与过期先结算由进程内用例覆盖。
- 上述结果只证明所列用例覆盖的路径，不构成部署验收。

## 10. 未验证与证据边界

- 数据只来自单机 Compose、2 副本、4 个 turn 的小样本；没有长会话、大工具输出、
  高并发写入的样本。
- 「写放大」是**按行数推断**，不是压测结论；没有测行/秒或 WAL 体积。
- `thought` 在官方事件集中缺失是文档结论，未逐条核对官方 33 种事件表的最新版本。
- 未验证 Rivet actor 驱逐/跨副本迁移下的回放行为；跨实例在途接管本就是设计非目标。
- 未验证「网关不能读会话库」是否为文档原话；该结论由库边界规则与 `session/load`
  回放帧丢弃行为推导。
