# Peri Swarm 官方文本 delta 验收（2026-11-26）

> 状态：**已验收（有限范围）**。代码基线 `72c10cfe9`（`feat/deploy-compose`，父 1
> `83611be4e`、父 2 `588c16693`）。本文只验收 C=2 的
> `agent.session.turn.output_text.delta` 两条路径（实时逐 chunk、回放单条全文）以及
> “持久化与行形状未变”；§5 的未验证项不构成验收。
>
> 相关契约：[peri-swarm.md §4.2 / §4.2.1 / §9](../design/peri-swarm.md)；
> 前一批验收：[Agents API 兼容与 item 粒度](peri-swarm-agents-compat-item-granularity-2026-11-26.md)；
> active spec：[事件日志粒度](../../spec/issues/2026-11-26-swarm-event-log-granularity.md)。

## 1. 基线与范围

| 项 | 值 |
| --- | --- |
| 实现提交 | `588c16693`（`feat/swarm-c2-delta`，单提交） |
| 合并提交 | `72c10cfe9`（`feat/deploy-compose`，`--no-ff`；父 1 `83611be4e`、父 2 `588c16693`） |
| 合并规模 | 13 文件，+1067/−172；不含 `deploy/**`、`docs/verification/**`、`peri-swarm/migrations/**`、`peri-swarm/src/store/**` |

本文的记录提交只改文档与 `sdk_sessions_e2e.mjs` 的一处文案，不改运行时逻辑、测试断言或
部署配置。

范围内行为：

- **实时**：root assistant `message` 的每个 chunk 发一条官方
  `agent.session.turn.output_text.delta`；首 chunk 先补 `item.added`、
  `content_part.added`，完成行落库后只补 done 系列，不再重复 added。
- **回放**：每条 assistant 完成行合成**一条全文** delta，顺序为
  `item.added` → `content_part.added` → `output_text.delta` → `output_text.done` →
  `content_part.done` → `item.done`；窗口从 item 中间开始时同序，不补历史 chunk。
- **未变**：`swarm_items` 仍是每 item 至多一行，不收 chunk；无 schema、迁移与
  `deploy/**` 改动；`/items` 投影未改。
- **不在范围**：reasoning 官方 delta（`reasoning_summary_text.delta` 等）未实现，
  `thought` 仍只有自定义 `item`。

## 2. 证据

> 来源：`.peri/plans/swarm-c2-delta-nopersist.md`（实现与测试证据）、
> `.peri/plans/swarm-c2-merge-deploy.md`（合并、构建、部署与冒烟证据）、独立验证结论
> （本轮由调用方提供，结论 PASS）。这些材料在 Git 忽略目录内，不随仓库发布；原始日志
> 落在 `/tmp/peri-swarm-c2-*`，可能已清理。**本轮未重跑部署或 Rust 测试。**

### 2.1 测试与 PostgreSQL 契约

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| `cargo fmt -p peri-swarm -- --check` | 退出 0 | 合并报告 |
| clippy（`-p peri-swarm --all-targets -- -D warnings`） | 退出 0，0 warning | 合并报告 |
| crate 测试（合并基线） | lib **421 passed / 0 failed / 20 ignored**（441）；集成 **115 passed / 0 failed / 7 ignored**（122）；doctest 0；合计 **536 passed / 0 failed / 27 ignored** | 合并报告，`/tmp/peri-swarm-c2-gates/test.log` |
| PostgreSQL 契约（显式 ignored） | **20 passed / 0 failed**；运行后 `swarm_it_%` schema 残留 0 | 同上，`/tmp/peri-swarm-c2-gates/pg.log` |
| 新增/加强用例逐个 `--exact` | 7 次，每次 1 passed / 0 failed | 实现报告 |
| 反向验证（实施阶段） | 5 条临时改动，均按预期失败；恢复后全量重跑通过 | 实现报告 §测试与判别力 |
| 独立验证 | **PASS**：范围/持久化未变、实时 added 恰好一次、回放单条全文 delta、4 条反向验证、上述 lib/it/ignored 与 PG 计数 | 独立验证结论（本轮调用方提供） |

27 个 ignored 中已显式运行 20 个 PG 契约；其余 7 个未运行。
独立验证的 4 条反向验证未随结论提供逐条改动明细，本文不复述其内容。

### 2.2 镜像与容器

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| 镜像 | `sha256:4195699eb3a9d9a659484cffca3a846d2305edc88570f5bbd5f1150d10281888`（210,097,405 B） | 部署报告，`/tmp/peri-swarm-c2-rebuild/image.id` |
| 上一版镜像 | `sha256:c6e6ea7ce229501f827d89e4c690b2498a2ae7f7681982f51a211c69fe3f1b96` | 同上 |
| 副本 1 / 副本 2 | running / **healthy**（`9949840901a6`、`325b77a7213b`），`.Image` 均为新镜像 | 同上，`docker compose ps` / `inspect` |
| `/healthz` / `/health` | 200 `{"status":"ok"}` / 401（缺少部署 Bearer 的预期拒绝） | 同上 |

`docker compose up -d --no-build` 退出 0，只重建两个 `peri-swarm` 副本。部署报告的
就绪轮询脚本因筛选条件把无 healthcheck 的 nginx/rivet-engine 误判为未就绪而打印
`TIMEOUT after 180s`；`ps` 与 `inspect` 确认两个目标副本在轮询期间已是 healthy。这是
脚本判定问题，不是容器故障，期间未重启服务。

### 2.3 真实 turn（经 nginx，既有 provider 与既有配置）

会话 `c2-smoke-90560d784e0c45a9`，turn `turn_01a21b2d-6280-7bb3-9402-55e3eb7b7a15`，
耗时 1.82s：

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| 终态 | `turn.completed`；助手输出 336 字符，含脚本标记 | 部署报告，`/tmp/peri-swarm-c2-rebuild/smoke3.log` |
| 实时 delta | **85 条** chunk delta，拼接 336 字符，首 chunk 3 字符；assistant item 恰好 1 次 `item.added`、1 次 `content_part.added`；完成后不再 added | 同上 |
| 回放 delta | `afterSeq=0` 下该 assistant item 恰好 **1 条** `output_text.delta`，`delta` 336 字符，与 `item.done` 的 `output_text` 逐字节相等；`event_id=evt_item_3_text_delta` | 同上 |
| 回放顺序 | `item.added` → `content_part.added` → `output_text.delta` → `output_text.done` → `content_part.done` → `item.done`（日志缩写作 `added->added->delta->done->done->done`） | 同上 |
| 落库 | `swarm_items` **3 行**（`message` 2 + `thought` 1），3 个唯一 `item_id`，**0 行持久 delta**；assistant 完成行**恰好 1 条**（`kind=message AND payload.role=assistant AND payload.complete=true`） | 同上，删除前只读事务 |
| 删除 | `DELETE` 200 → `GET` 404 → 会话/turn/item 行均为 0 | 同上 |

脚本对实时流逐项断言“added/part.added 恰好一次、delta 单调、拼接等于全文”；冒烟只跑了
1 个成功 turn，粒度（85 chunk、首 chunk 3 字符）由 provider 决定。

### 2.4 持久化未变

- `swarm_items` 行数与行形状未因 delta 改动增加：本次冒烟 3 行，与前一批验收的单 turn
  观察相同（2 `message` + 1 `thought`），持久 delta 行 0。
- 回放只读完成行，不读 chunk 行或 `swarm_turns` 的新字段；未新增 schema、迁移或列。
- 官方帧是叠加层：既有自定义 `item` / `turn` / `delta` 帧保持不变。

## 3. 逐项契约

### 3.1 帧顺序

| 场景 | 帧顺序 |
| --- | --- |
| 实时首 chunk | 自定义 `item` → `turn.item.added` → `turn.content_part.added` → `turn.output_text.delta` |
| 实时后续 chunk | 自定义 `item` → `turn.output_text.delta` |
| 实时完成行（有实时 chunk） | 自定义 `item` → `output_text.done` → `content_part.done` → `item.done`；不再 added |
| 实时完成行（无实时 chunk） | 自定义 `item` → `item.added` → `content_part.added` → done 系列（原行为） |
| 回放 assistant 完成行 | 自定义 `item` → `item.added` → `content_part.added` → 单条全文 `output_text.delta` → `output_text.done` → `content_part.done` → `item.done` |
| 回放自 item 中间开始 | 同上一行；不按被截断的历史 chunk 行补帧 |
| `thought` / child / `recovered` | 只有自定义 `item`；无任何官方文本帧 |

证据：合并入的新增/加强用例（逐个 `--exact` 共 7 次）与既有序列用例
（`a_single_turn_keeps_the_official_sequence`、HTTP SSE 顺序、actor runtime live/replay
对比）；部署观察覆盖纯文本单 turn 的实时与回放。

### 3.2 字段

- SSE `event:` 名与 JSON `type` 同为 `agent.session.turn.output_text.delta`。
- JSON 字段（snake_case）：`event_id`、`session_id`、`turn_id`、`item_id`、
  `content_index`（恒 `0`）、`output_index`、`delta`。
- 实时 delta 帧没有 `seq` / `id`，不落库、不推进 `Last-Event-ID`；`item_id` 与完成行
  相同，用于把 chunk 关联到合并 item。
- 实时首 chunk 的 `item.added.item` 携带首 chunk 文本；该 payload 语义见 §5 第 4 条。

### 3.3 `event_id` 规则

| 帧 | 实时 | 回放 | 跨投递 |
| --- | --- | --- | --- |
| assistant `item.added` | `evt_item_<itemId>_added` | 同左 | 一致 |
| `content_part.added` | `evt_item_<itemId>_part_added` | 同左 | 一致 |
| `output_text.delta` | `evt_item_<itemId>_delta_<chunk>`（chunk 从 1 起，流内计数） | `evt_item_<seq>_text_delta` | **不保证一致** |
| `output_text.done` / `content_part.done` / `item.done` | `evt_item_<seq>_*` | 同左 | 一致 |

实时 chunk 没有持久 `seq`，因此 delta 的 `event_id` 只能来自进程内 chunk 序号；回放用
完成行 `seq`。assistant added/part.added 的 `event_id` 只由 `(item_id, 帧类型)` 决定，
不含序号或时钟。跨重连按 `event_id` 去重时必须排除 delta，或接受同一文本两次 delta。
assistant added/part.added 已改为按稳定 `itemId` 派生，跨投递一致（实施报告的反向验证
“回放 assistant added 改回 `seq` 派生”会让该一致性用例失败）。

### 3.4 `output_index` 与完成行

- 首 chunk 打开 root assistant 流时在内存分配 `output_index`，`begin_turn` 归零；
  user item、`Thought` 与 child 行不参与，也不消耗序号。
- 完成行把同一值写入持久 payload 的 `outputIndex`；实时帧与回放帧都只读这一事实，不维护
  第二套推导。旧 payload 缺该键时，回放按 turn 内完成行顺序推导。
- 官方帧字段名是 `output_index`，持久 payload 键是 `outputIndex`，两者值一致；
  `item.added` 的 `output_index` 仍为 `null`，完成序号只由 delta/done 系列与 `item.done`
  承载。
- 证据：`live_chunks_emit_one_official_item_and_three_deltas` 断言完成行沿用首 chunk 的
  `outputIndex`；回放用例断言 delta 的 `event_id` 使用完成行 `seq`（冒烟为 `seq=3`）。

### 3.5 `thought` 不发官方帧

- `Thought` 流共享 answer 的 `messageId`，但流键含 `kind`，只产生自定义 `item` /
  自定义 `delta`；完成行持久化但不带 `phase`/`outputIndex`，不进入
  `turn.item.added`/`item.done`。
- reasoning 官方帧本轮**未实现**：没有 `reasoning_summary_text.delta` / done。
- child（`projection child:<id>`）与 `recovered` 行同样不产生官方文本帧；实现报告的
  反向验证“去掉 `active && turn_id` 门控”会让 recovered chunk 产生官方帧并失败。

## 4. 与既有 item 粒度的关系

- 回放仍按持久 `seq` 逐行发自定义 `item`；新写入每个 `itemId` 只有一条完成行，历史多行
  item 仍由客户端按 `itemId` 折叠。
- 只有 `complete:true` 的 root assistant 行合成 delta；`/items` 投影与合并落库裁决不变。
- 前一批验收记录的未验证项（真实断线重连、审批折叠、Node e2e 等）不受本轮影响，仍未验证。

## 5. 未验证与限制

1. **reasoning 官方 delta 未实现**：`reasoning_summary_text.delta` 等 4 种帧都不发；
   `thought` 只有自定义 `item`（`reasoning.summary`）。这是本轮范围外，不是回归。
2. **回放没有原始 chunk 边界**：回放无论原始 chunk 多少都只发一条全文 delta，依赖逐
   chunk 重放的客户端只能消费实时帧。
3. **历史同 `itemId` 多条 `complete:true` 行**：回放会对每条完成行各发一条该行全文
   delta（`push_assistant_item` 逐行调用），客户端按 delta 拼接会得到两段全文，与单条
   完成行的实时语义不同。该形状只存在于旧数据，库内当前无样本，**无测试覆盖**，也未在
   部署复现。
4. **实时 `item.added` 携带首 chunk 文本**：若客户端既按 `event_id` 去重 added、又把
   added 文本与 delta 拼接，首段可能重复；若只拼接 delta 则正常。该交互未用真实官方
   SDK 或客户端验证，只是契约层面的风险提示。
5. **关闭检查与广播之间的窄竞态**：`stage_item` 检查 `closed`，`flush_staged` 在广播前
   释放锁复查并标记 `live_broadcast`，但复查与 `broadcast` 之间仍有窗口；并发终态
   flush 时可能有一条 delta 排在完成帧之后。该竞态**既有，非本轮新增**，未注入并发复现。
6. **Node e2e 未在本基线运行**：`peri-swarm/scripts/sdk_sessions_e2e.mjs` 的过时文案已
   改为如实描述，但脚本行为与断言未改，也未重跑；真实官方 SDK 未消费实时 delta 事件。
7. **部署只覆盖 1 个成功 turn**：取消中的 delta、lagging 订阅的实时 delta、长流背压与
   工具/审批 turn 的帧交错未在部署环境重放；这些只有合并入的进程内用例覆盖。
8. **空文本首 chunk**：仍会打开流并占用一个 `outputIndex`；若该流后续始终为空，后续消息
   的序号可能出现空洞。极端输入未测，不影响行数或 schema。
9. **副本路由未记录**：未单独记录冒烟请求命中哪个副本；两个副本均在同一新镜像上 healthy。

## 6. 证据局限

- 测试计数、镜像 ID、容器状态与单 turn 数字均引自实现/部署报告，**本轮未重跑**；
  独立验证只提供结论与计数，未提供逐条反向验证明细。
- 27 个 ignored 中只显式跑了 20 个 PG 契约；其余 7 个未运行，不能据此声称 Rivet、
  两库部署或 ACP 测量覆盖。
- 部署是单机 Compose、两个副本、一个真实 provider 的一个纯文本 turn；85 chunk 是
  该 provider 的粒度，换模型/网关会不同；回放始终只有 1 条全文 delta。
- 实现报告曾把“HTTP SSE 实时 delta 未用真实模型独立跑”列为缺口；部署冒烟经 nginx
  观察到 85 条实时 delta，已覆盖该路径的一个样本，但仍是一个 turn，不是长会话或高并发。
- 容器/镜像 ID 是 2026-11-26 的时点值；重新构建会得到不同 ID。

## 7. 结论

C=2 在所列证据范围内符合契约：实时逐 chunk delta（85 条、拼接等于 336 字符全文、
added/part.added 恰好一次）、回放单条全文 delta（1 条、与 `item.done` 逐字节相等、
顺序正确、`event_id` 用完成行 `seq`）、持久化与行形状未变（3 行、0 delta 行、
1 条 assistant 完成行），并有 lib 421 / it 115 / 27 ignored 与 PG 20 的基线测试通过。

本文不声称完整验收：reasoning delta、历史多完成行、added 首段去重交互、关闭-广播窄
竞态、Node e2e 与部署环境下的取消/背压路径仍未验证（§5）。
