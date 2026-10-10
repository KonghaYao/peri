# peri-cf 聊天对象 P3 批准包（待逐项裁决）

状态：**六项已全部批准，推荐方案已实施**（2026-10-10）。实施结果与验证见 [实施 issue](2026-10-10-peri-cf-chat-objects-implementation.md) 的「P3 交付记录」；本文件保留为决策与证据记录，附录 A/B 的一次性探针脚本运行后已删除（源码留档）。
批准口径：①`recordVersion:2` 单键索引 + `payload:<id>:<n>` 1 MiB 分段 + 7 天 `record:v1-backup`；②DO 分段存全文、零新增平台资源；③段 1 MiB / 单载荷 8 MiB / 会话 32 MiB / 索引 1 MiB / interaction 50 / 任务 200；④无时间保留、按引用回收 + 孤儿清扫 + 备份 7 天；⑤零新增资源；⑥`/api/v2/chats/...`（含 `/sync`）+ 维护窗口 + 回滚演练。
依据：[已批准设计](../../docs/design/peri-cf-chat-objects.md) 5.2 / 5.3 / 6.2 / 9 节；实施进度见 [实施 issue](2026-10-10-peri-cf-chat-objects-implementation.md)。

## 0. 核实证据（全部可复核）

### 0.1 官方文档（2026-10-10 抓取）

| 事实 | 数值 | 来源 |
| --- | --- | --- |
| DO（SQLite 后端）单值上限 | key 与 value 合并 ≤ **2 MB** | <https://developers.cloudflare.com/durable-objects/platform/limits/>（页面标注 Last updated Jun 1, 2026） |
| DO 每对象存储 | **10 GB**（Paid）；账户内不限 | 同上 |
| DO SQL 单行/字符串/BLOB | ≤ 2 MB | 同上 |
| DO WebSocket 接收消息 | 32 MiB | 同上 |
| DO 每请求 CPU | 30 s 默认，可配置至 5 min | 同上 |
| DO 存储语义 | 每个方法隐式事务（原子且隔离）；未 `await` 的连续 put 自动成组提交，机器故障时全有或全无；`put` 返回 Promise，`await` 施加背压 | <https://developers.cloudflare.com/durable-objects/api/storage-api/> |
| 旧 KV 后端（本项目未用） | key 2 KiB / value 128 KiB | limits 页 |
| R2 | 单对象 ≤ 5 TiB；单次上传 5 GiB（单段）/ 4.995 TiB（多段）；元数据 8 KiB | <https://developers.cloudflare.com/r2/platform/limits/> |
| Workers 每请求同时出站连接 | 6 | limits 页 |

- 本项目 DO 使用 SQLite 后端：`npm-packages/peri-cf/wrangler.jsonc` 的 `migrations: [{ tag: "v1", new_sqlite_classes: ["ChatSession"] }]`，因此上表 SQLite 行是有效约束。
- **未核实**：部署端 2 MB 的实际强制点与错误形态（本地 workerd 未复现，见 0.2）；DO 写入速率/计费口径的完整细节。未经核实前不得把本地结果当作平台行为。

### 0.2 仓库内实测（一次性探针，已删除；源码见附录 A）

| 项 | 结果 |
| --- | --- |
| 本地 workerd（miniflare，`useSQLite: true`）单值写入 | 128 KiB、1 MiB、2 MiB、2 MiB+1、4 MiB、8 MiB 全部写入成功且读回一致 |
| 同环境 16 MiB / 32 MiB | 失败：`string or blob too big: SQLITE_TOOBIG` |
| 未 `await` 连续写入后抛错 | 两个键都持久化（异常不回滚已提交写入），与文档语义一致 |
| 本地与部署差异 | 本地未在 2 MB 拒绝；部署端 2 MB 应视为硬约束。本地证据只证明写入路径与失败形态，不替代平台验收 |

### 0.3 checkpoint 体积实测（构造样本，脚本见附录 B）

| 样本 | 索引部分（entries/tools/turns/…） | payloadValues | 最大单值 |
| --- | --- | --- | --- |
| A：500 轮 × 20 字符 | 355 KB | 0 | — |
| B：50 轮 × ~1 KB | 329 KB | 0 | — |
| C：50 轮 + 1×4 MiB 工具结果 | 330 KB | 4.19 MB | 4.19 MB |
| D：50 轮 + 32×128 KiB 工具结果 | 342 KB | 4.20 MB | 131 KB |
| E：10 轮 + 50 条 interaction | 27 KB | 0 | — |

结论：**索引部分稳定在 ~0.3 MB**（远低于 2 MB 单值上限）；超限风险全部集中在 payloadValues——单载荷上限 8 MiB > 2 MB，会话外置预算 32 MiB ≈ 32 MB。

### 0.4 现状代码事实

1. DO 持久化：`worker/chat/session.ts::save()` 为单值 `storage.put("session", record)`；`MAX_TRANSCRIPT_BYTES = 1 MiB` 在接纳新 prompt 时拒绝超限会话。
2. 展示面已就绪：`SessionDocs.exportCheckpoint()/fromCheckpoint()`（`checkpointVersion = 1`，载荷全文随 `payloadValues` 导出；未知版本/未知 pair/缺值与错配引用显式拒绝）；HTTP 首屏与 WS 推送共用 `projection.state()`；`readiness` 与 `UnsupportedSchemaError` 已在浏览器与首屏两侧接线。
3. 预算常量（均「默认待确认」）：inline 4096 B、preview 512 字符、inline 累计 1 MiB、单载荷 8 MiB、会话外置 32 MiB、interaction 50 条；任务上限 200（`task-projection`）。
4. 浏览器读取：`readPayload` 单次展示 1 MiB、并发 2（默认）；默认 reader 明确报告端点未提供。
5. WS 交付信用（应用侧已按平台调整过的数字）：`MAX_OUTSTANDING_BYTES 4 MiB`、16 帧、总计 8 MiB。
6. 过渡物：`legacyIdentity` 旧记录身份桥（仅 restore 配对）；旧记录写回仍为纯文本行。

## 1. DO 记录结构切换

**性质**：用户已批准方向（display checkpoint 取代纯文本 messages），**只差结构确认**；回滚语义与索引上限属全新决策。

**现状**：单键 `"session"` 存 `{chat, messages[], running, executionBlocked}`，文本行由应用物化；重启恢复依赖 `legacyIdentity`；展示身份/时间/错误虽已由 SDK 提供，但持久化仍是旧形状。

**选项**

- **1A（推荐）单键索引 + 分段载荷**：`record` 键存索引（含 `recordVersion`），payload 全文按 ≤1 MiB 分段存 `payload:<id>:<n>`。
- **1B 索引在 DO、载荷在 R2**：见 §2 的 P2；结构同上，但 parts 由对象存储承担。
- **1C 旧行 + checkpoint 双写**：违反设计 5.2.8（不长期双写），不推荐。

**结构草案（1A）**

```jsonc
// key: "record"
{
  "recordVersion": 2,
  "chat": { "id": "…", "title": "…", "updatedAt": "…" },        // 应用元数据（与 Rust Store 权威一致）
  "running": false, "executionBlocked": false,
  "checkpoint": { "checkpointVersion": 1, "schemaVersions": { "chat": 3, "session": 2 },
                  "session": {}, "entries": [], "toolCalls": [], "turns": [], "tasks": [],
                  "plansByTurn": {}, "interactions": [], "payloadIndex": [ { "id": "…", "bytes": 4194347, "parts": 5 } ] }
  // checkpoint.payloadValues 置空：全文不进单值
}
// key: "payload:<payloadId>:<n>"  → string（≤ 1 MiB/段，UTF-16 安全切分后校验字节）
// key: "record:v1-backup"         → 转换前的旧记录（见回滚）
```

- 读取：`readPayload(id)` 拼接全部段 → 校验总字节数与 ref 一致 → 返回；任一段缺失返回 unavailable（绝不返回其它版本内容）。
- 写入顺序（对齐设计 5.3.5）：全部分段写成功 → 段数/字节校验 → 更新 `record` 索引提交；失败不宣称引用已耐久。
- 旧记录转换：应用解析旧行 → 构造 import 输入（保留旧 id/time，error 安全转换，纯文本只生成 text，已丢 reasoning/tool/usage 不补造）→ `SessionDocs.fromCheckpoint` → 导出新 checkpoint → 先写段、再写 `record`；`legacyIdentity` 的移除条件：转换完成且写回成功（新记录不再含旧行）。
- 回滚预检：转换前把旧记录整体复制到 `record:v1-backup`；转换后校验（readiness=ready、条目数与旧行一致、抽样 payload 可读）再删除旧键 `session`。回滚 = 部署上一构建 + 还原备份键（窗口内），窗口后只支持前滚修复。

**推荐**：1A + 备份键。理由：索引实测 ~0.3 MB，单值余量充足；分段把单值压在 1 MiB（< 2 MB 平台上限，且留 2× 余量）；无需新平台资源；写入顺序与设计一致。

**影响/成本/风险**

- 成本：每次载荷替换多 `ceil(bytes/1 MiB)` 次 storage 写（8 MiB 单载荷 ≤ 8 段 + 1 次索引）；restore 读放大多一次列表遍历。
- 风险：段与索引非原子（5.3.6 已记录）→ 用写入顺序 + 孤儿清扫收敛；`record` 索引超 1 MiB 上限时需拒绝新 turn（§3）；备份键在窗口内增加 ≤1 MiB/会话。
- 未核实：部署端 2 MB 上限的错误形态（本地未复现），需在预发/生产验证一次写入边界。

**批准后步骤**

1. 冻结 `recordVersion: 2` 草案与索引上限（本轮裁决 1、2、3 项）。
2. 在受控分支实现 `recordVersion` 读写适配器与转换器（含 1 MiB 段切分与校验、孤儿清扫），先补契约测试（转换幂等、段缺失、索引提交失败、回滚还原）。
3. 预发验证单值/总量/CPU 边界后，再走 §5 维护窗口。

**不批准的替代路径**：维持纯文本行（当前状态）——展示面继续损失 reasoning/tool/usage 的历史往返，重启后身份只能粗粒度恢复；功能上可继续运行，但设计第 8 节的“单写新权威”目标无法达成。

## 2. payload 持久化

**性质**：方向已批准（外置 `{id,bytes,preview}` + 浏览器经鉴权可达），**结构/选型确认属全新决策**。

**现状**：全文仅内存，SDK checkpoint 可携带全文；DO 侧无持久化；浏览器端点未提供（默认 reader 明确报告不可用）。

**选项**

- **P1（推荐）DO 分段**：沿用 §1 的 `payload:<id>:<n>`。零新资源；受 2 MB/值约束（1 MiB 段安全）；每会话 32 MiB ⇒ ≤32 段；读取经 DO（CPU 计入请求预算）。
- **P2 R2 对象**：`chatId/payloadId` 单对象（≤5 TiB；单段上传 ≤5 GiB）。优点：大载荷不占 DO 限额与 CPU、天然支持 Range 流式下载；代价：新 bucket + 绑定（§4）、一致性/鉴权/超时新失败面、需要 `ctx.waitUntil` 协调写入顺序。
- **P3 混合**：小载荷 DO、>1 MiB 走 R2。复杂度高，暂不推荐。

**推荐**：先 P1，触发条件后再评估 P2。触发条件（任一）：单载荷常态 >8 MiB 或需要 Range 流式下载；每会话持久量成为常态；DO 写入/CPU 观测显示成本；产品引入附件/长期保存。

**影响/成本/风险**：P1 下浏览器读取路径经 DO（`GET /api/v2/chats/:id/payloads/:refId` 一次性或分块），在 1 MiB 展示上限内单次请求即可（≤1 MiB 全量 + JSON 转义），不整包 `JSON.parse` 大对象；超过展示上限的走鉴权流式下载（P2/P3 能力）。失败码沿用 SDK 契约（`unauthorized`/`unavailable`/`too_large`/`integrity_error`/`aborted`/`transport_error`）。

**批准后步骤**：定 §3 数值 → 实现 DO 分段存储与读取端点（鉴权 + `Cache-Control: no-store`）→ 应用把 SDK reader 接到该端点 → 契约测试（越权 ref、跨 chat、字节不符、缺段、超限、取消）→ 预发验收。

**不批准的替代路径**：继续只存索引不存全文（当前状态）——UI 只能显示 preview，`readPayload` 永远 unavailable；不违反设计（“不允许已成功接纳但永久无法读全文”要求的是：要么验收下载路径，要么按上限明确拒绝），但 32 MiB 会话预算就失去意义，可把预算调小直至批准。

## 3. 容量、保留期与 GC

**性质**：**全新决策**（设计 6.2.7 只给评审起点：1 MiB 单次展示、并发 2）。

**现状/既有默认**：单载荷 8 MiB、会话外置 32 MiB、inline 累计 1 MiB、inline 分界 4096 B、preview 512 字符、interaction 50 条、任务 200 条、WS 信用 4/8 MiB、旧记录接纳上限 1 MiB。

**建议默认值（可在批准时逐项调整）**

| 项 | 建议值 | 依据 |
| --- | --- | --- |
| 载荷分段 | 1 MiB | 部署端单值 2 MB 的一半，留余量；实测索引 ~0.3 MB |
| 单载荷 | 8 MiB（沿用） | 已实现常量；对应 ≤8 段 |
| 会话外置累计 | 32 MiB（沿用） | 已实现常量；对应 ≤32 段、DO 侧 ≤33 MiB/会话 |
| checkpoint 索引 | ≤1 MiB | 与旧记录接纳上限一致；实测 500 轮仅 0.35 MB |
| interaction / 任务 | 50 条 / 200 条（沿用） | 已实现常量 |
| 浏览器单次展示 / 并发 | 1 MiB / 2（沿用） | 设计评审起点 |
| 保留期 | **无时间保留**；按引用生命周期回收 | 见 GC |
| 备份键窗口 | 7 天 | §1 回滚用；单会话 ≤1 MiB |

**回收策略**

1. 版本被替换或 rewind 清除：立即删除旧段（SDK 已做 values 退役，DO 侧按同一事件删除段）。
2. checkpoint 重写后索引中不再出现的 payload id：删除其所有段。
3. 孤儿段（存在 `payload:` 键但索引无引用，例如段写成功而索引提交失败）：在会话加载、转换或每日一次机会性扫尾时回收；回收失败仅记日志，不阻塞会话。
4. 删除整会话（未来特性）：`storage.deleteAll()` 归该会话 DO；当前无删除入口，不新增。

**失败处理**：写段失败 → 该载荷整体视为未接纳（沿用 SDK 的容量拒绝语义），不写索引；索引提交失败 → 旧 checkpoint 保持可读，段留待清扫；读取遇缺段 → `unavailable` + 日志，不返回其它版本内容，不伪装成功。

**批准后步骤**：确认上表数值 → 落地常量与 GC 任务 → 测试（替换回收、孤儿清扫、缺段、索引上限拒绝）→ 预发压测（大载荷写入耗时/CPU 计入）。

**不批准的替代路径**：把上限整体下调（例如单载荷 1 MiB、会话 8 MiB）即可在不新增机制的情况下降低风险；或暂时只保留索引（同 §2 替代路径）。

## 4. 平台资源清单

**性质**：P1 路线为零新增；R2 属**全新决策**。

| 方案 | 需创建 | 绑定/配置 | 影响 |
| --- | --- | --- | --- |
| P1（推荐） | **无** | 无（沿用 `CHAT_SESSIONS`，SQLite 后端迁移已存在） | 无部署变更；仅代码与存储键变化 |
| P2 | 1 个 R2 bucket（建议名 `peri-chat-payloads`，私有、指定位置提示） | `wrangler.jsonc` 新增 `r2_buckets` 绑定（如 `CHAT_PAYLOADS`）、`worker/types.ts` Env、`cf:typegen` 重生成、可选对象生命周期规则 | 账号新增资源；部署与权限变更；回滚=移除绑定（对象数据需单独备份） |

**本轮不创建任何资源。** 若选 P2，建议作为独立小项批准（bucket 名称、区域提示、是否配生命周期规则），与 §1 解耦推进。

## 5. 3/2 升级路径

**性质**：方向已批准（版本化入口 + 短维护窗口 + 消费端拒绝 + 不长期双写）；入口命名与演练步骤属**实施设计**，回滚语义属全新决策。

**现状**：SDK 常量 `CHAT_SCHEMA_VERSION = 2`、`SESSION_SCHEMA_VERSION = 1`；checkpoint `SUPPORTED_SCHEMA_VERSIONS = {chat:2, session:1}`；同步 wire `SYNC_WIRE_VERSION = 2`、DO socket attachment 版本 2。应用入口：`GET /api/chats/:id`、`/resources`、`POST /messages`、`/cancel`、`WS /sync` + SPA 静态资源。

**入口选项**

- **E1（推荐）路径版本化**：新增 `/api/v2/chats/...`（含 `/sync`）作为新版消费面；切换后旧路径返回 `426 Upgrade Required`（JSON `{error, upgrade:true}`）或对 WS 以 `1011/1008 + "Client upgrade required"` 关闭。SDK 同步 wire 不变。
- **E2 同路径版本头/子协议协商**：改动最小，但同一路径承载两种语义，代理/缓存/排障边界不清晰；旧客户端不带头时依赖默认分支。
- **E3 独立域名（`v2.`）**：隔离最强，部署/证书/回滚成本最高，仅当需要并行灰度环境时考虑。

**维护窗口步骤（对齐设计 5.2.7）**

1. 冻结：`POST /messages` 返回 503 + “升级中”文案（不接纳新 turn）。
2. 结算：等待在途 turn 结束；宿主关闭未确认的会话保持 blocked，不强制重跑。
3. 断连：以明确 reason 关闭旧 socket（客户端按 failed 处理，不循环 resume——`ChatSync` 已实现）。
4. 转换：逐会话执行 §1 转换（建议惰性 on-load + 一次性扫描兜底；转换幂等）。
5. 切换：部署新构建；旧路径返回升级错误；SPA 新构建指向 v2 路径。
6. 验证：抽样 `readiness=ready`、条目数与旧行一致、payload 可读、无 `legacyIncomplete`。

**回滚**：部署上一构建；若会话已被转换，需在备份窗口内还原 `record:v1-backup`（§1 R1），窗口后只能前滚修复。升级演练必须包含一次这样的回滚。

**旧客户端**：收到显式升级错误或断连；不伪装空历史（SDK `readiness=unsupported` + 应用 `UnsupportedSchemaError` 已接线）。

**批准后步骤**：确认 E1 → 定 v2 路由名与错误文案 → 更新 SDK 常量与 checkpoint 支持集（chat3/session2）并让旧 pair 在新 view 中判 unsupported → 预发演练（含回滚）→ 生产窗口。

**不批准的替代路径**：保持 2/1（当前状态）——resource 块与消费端拒绝校验不落地，设计目标未完成，但功能可用；需要时把 P2/P3 继续留待后续。

## 6. 逐项性质标注

| 项 | 已批准方向（只差结构确认） | 全新决策 |
| --- | --- | --- |
| 1 DO 记录切换 | checkpoint 取代纯文本、import 语义、写入顺序 | 记录 JSON 形状、`recordVersion: 2`、索引 1 MiB 上限、回滚备份窗口 |
| 2 payload 持久化 | 外置引用 + 浏览器鉴权可达 + 写入顺序 | 分块 vs R2、段大小、是否新增对象存储 |
| 3 容量/保留/GC | 评审起点（1 MiB 展示、并发 2、4096 B 分界） | 具体数值、无时间保留、孤儿清扫策略 |
| 4 平台资源 | —— | 是否创建 R2 bucket 与绑定 |
| 5 3/2 升级 | 版本化入口、短维护窗口、不长期双写、消费端拒绝 | 入口命名（E1/E2/E3）、回滚还原机制 |

## 7. 需要用户逐项裁决的最小清单

1. **DO 记录结构**：采用「`recordVersion: 2` 单键索引（含 chat/running/executionBlocked/checkpoint）+ `payload:<id>:<n>` 1 MiB 分段」草案？（§1）
2. **payload 持久化**：先用 DO 分段、暂不引入 R2？（§2）
3. **容量数值**：分段 1 MiB、单载荷 8 MiB、会话外置 32 MiB、索引 ≤1 MiB、interaction 50、任务 200？（§3）
4. **保留与 GC**：无时间保留、按引用回收 + 孤儿段机会性清扫（并保留 7 天 `record:v1-backup`）？（§3）
5. **平台资源**：本批零新增；若未来选 R2，单独立项批准 bucket 与绑定？（§4）
6. **升级入口**：采用路径版本化 `/api/v2/chats/...`（含 WS）与 §5 维护窗口步骤，并演练一次回滚？（§5）

每项可选「接受推荐 / 调整数值 / 推迟」；若全部推迟，则 P3 整体维持现状（功能可用，设计目标未完成）。

## 附录 A：本地 DO 存储探针（一次性，已删除）

```ts
// npm-packages/peri-cf/tests/tmp-p3-storage-probe.test.ts（运行 bun test 后删除）
import { afterAll, beforeAll, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";

const script = `
export class Probe {
  constructor(ctx) { this.ctx = ctx; }
  async fetch() {
    const results = {};
    const probe = async (label, bytes) => {
      try { await this.ctx.storage.put("probe:" + label, "x".repeat(bytes)); results[label] = { ok: true }; }
      catch (error) { results[label] = { error: String(error && error.message), name: String(error && error.name) }; }
    };
    for (const [label, bytes] of [["kiB128", 128*1024], ["mib1", 1024*1024], ["mib2", 2*1024*1024],
      ["mib2_plus1", 2*1024*1024+1], ["mib4", 4*1024*1024], ["mib8", 8*1024*1024],
      ["mib16", 16*1024*1024], ["mib32", 32*1024*1024]]) await probe(label, bytes);
    try { this.ctx.storage.put("batch:a", "a"); this.ctx.storage.put("batch:b", "b"); throw new Error("after-write"); }
    catch { results.threwAfterWrite = true; }
    results.batchA = await this.ctx.storage.get("batch:a");
    results.batchB = await this.ctx.storage.get("batch:b");
    return Response.json(results);
  }
}
export default { async fetch(request, env) { return env.PROBE.get(env.PROBE.idFromName("probe")).fetch(request); } };
`;

let runtime: Miniflare;
beforeAll(async () => {
  runtime = new Miniflare(convertV4MiniflareOptions({ name: "p3-storage-probe", modules: true, script,
    compatibilityDate: "2026-10-01", durableObjects: { PROBE: { className: "Probe", useSQLite: true } } }));
  await runtime.ready;
}, 30_000);
afterAll(async () => { await runtime?.dispose(); });
test("probe", async () => {
  const results = await (await runtime.dispatchFetch(new URL("/probe", await runtime.ready))).json();
  console.log("PROBE_RESULTS", JSON.stringify(results, null, 2));
  expect((results as { kiB128: { ok?: boolean } }).kiB128.ok).toBe(true);
}, 30_000);
```

结果：`mib8` 及以下全部 `{ok:true}`；`mib16`/`mib32` 为 `SQLITE_TOOBIG`；`batchA/batchB` 均为 `"a"/"b"`。

## 附录 B：checkpoint 体积脚本（一次性，已删除）

`npm-packages/@peri-sdk/tmp-p3-checkpoint-size.ts`：用 `SessionDocs` 构造 A–E 样本（`compact_completed` 批量装载历史 + 真实 tool/交互写入），对 `exportCheckpoint()` 统计 `JSON.stringify` 字节数，并区分 `payloadValues` 与其余字段（把 `payloadValues` 置空后再测量）。输出即 §0.3 表格。
