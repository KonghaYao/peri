# MonitorMCP（command source 监控）

> **状态**：**已批准目标设计**——v1 command source、标准 MCP Tasks 通信与 P2
> alert 提醒已实现。提醒由宿主任务订阅轮询 `tasks/get` 的
> `_meta["peri/monitor"]` 驱动：只有 alert 命中唤醒 owner，任务终态不唤醒但会
> flush 尚未报告的命中。poll/file/WS source 不在范围。
>
> **事实源**：`mcp-packages/monitor/src/{server,state,buffer,tools}.rs`（handler、
> 任务状态机、事件 buffer、工具声明）；注册表与工具声明在
> `peri-acp-types/src/builtin_mcp.rs`；alert 契约在
> `peri-acp-types/src/monitor.rs`；宿主身份判定、投递策略与任务订阅在
> `peri-middlewares/src/mcp/client/{monitor_identity,monitor_alert,subscription_tasks}.rs`。
>
> **关联契约**：ARC-MIDDLEWARE-001、ARC-CAPABILITY-CLOSURE-001、ARC-TOOLS-001、
> ARC-CANCEL-001、[session-async-tasks.md](session-async-tasks.md)、
> [../reference/mcp-ecosystem.md](../reference/mcp-ecosystem.md)（MCP Tasks 与
> `subscriptions/listen` 口径）。进度与验收见
> [2026-11-01-monitor-mcp.md](../../spec/issues/2026-11-01-monitor-mcp.md)。

## 1. 范围与形状

MonitorMCP 是独立 builtin MCP 实例（server name / 配置 key / 目录键均为
`monitor`），与 `workspace` / `cron` 平级，不占用 middleware 槽位。v1 只有
command source：一条 shell 命令产生 stdout/stderr 行事件；MonitorMCP 不理解业务
数据，也不判断「什么变了」。

```text
   monitor_start / monitor_read / monitor_stop / monitor_list
agent ───────────────────────────────────────────────────────────────▶ MonitorMCP
  ▲                                                                       │
  │  MCP Tasks: tasks/get, tasks/cancel（标准方法，无自定义生命周期）       │
  │                                                                       ▼
  └── 宿主任务订阅轮询 tasks/get（每 2 秒）◀── _meta["peri/monitor"] ── MonitorState
            │                              {alertMatched, lastAlertSeq, label}
            │ alertMatched > processed
            ▼
      发起会话 inbox：Defer 提醒（30 秒合并；终态 flush）
                                        ┌─────────────────────────────────┤
                                        ▼                                 ▼
                                 shell command                      EventBuffer
                                 (stdout / stderr)                  (ring, seq)
                                                                          │ alert match
                                                                          ▼
                                                      alertMatched += 1 / lastAlertSeq
```

实例只持有内存状态。宿主或实例被替换后，旧任务 id 不再可解析；没有落盘、恢复或
重放通道。

## 2. 工具契约

四个模型工具全部 deferred（模型面名字 `mcp__monitor__*`），无 prompt 声明模板。
没有 host-only 工具：alert 提醒由宿主任务订阅读取 `tasks/get` 的 `_meta` 驱动，
不占用实例的工具面。

### 2.1 `monitor_start`

请求：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `command` | string | 必填，非空 shell 命令 |
| `cwd` | string? | 工作目录；相对路径相对实例 cwd 解析 |
| `label` | string? | 供 `monitor_list` 与 alert 提醒使用的短标签；≤256 字节 |
| `alert` | string? | 线性时间正则，≤512 字符；命中行累计到宿主提醒 |
| `deadline_ms` | integer? | 默认 3 600 000；范围 1..=86 400 000 |
| `max_events` | integer? | 默认 2 000；范围 1..=100 000 |
| `max_bytes` | integer? | 默认 1 MiB；范围 16 KiB..=64 MiB |

响应：标准 `CreateTaskResult`（`resultType: "task"`）。

- `task.taskId`、`task.status = working`、`createdAt`、`lastUpdatedAt`、
  `pollIntervalMs = 1000`；
- `_meta["peri/monitor"] = { startedAt, deadlineAt, label }`；
- 不返回事件正文；事件经 `monitor_read` 读取。

校验失败、命令无法启动或容量满时返回工具级错误，不注册任务。

#### 宿主回执中的两个 id

模型看到的不是上面的原始 `CreateTaskResult`，而是宿主 `tool_bridge` 的回执。
回执同时给出两个命名空间不同的 id，模型无需猜测：

- `task_id`（`mcp-<uuid>`）：宿主后台任务 registry 的 public id，只用于宿主任务
  管理与 Tasks 面板；monitor 实例和 MCP `tasks/get` 都不接受它。
- `monitor_id`（`monitor-<uuid>`）：monitor 实例内的 raw task id，也就是
  `monitor_list` 返回的 `taskId`。把它作为 `task_id` 参数传给 `monitor_read` /
  `monitor_stop`；MCP `tasks/get` / `tasks/cancel` 也用它（宿主按会话注入 owner）。

用 `task_id` 调 `monitor_read` / `monitor_stop` / MCP `tasks/get` 会得到
`unknown task`；操作 monitor 任务时必须使用 `monitor_id`（或 `monitor_list` 的
`taskId`）。

### 2.2 `monitor_read`

请求：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `task_id` | string | 必填；monitor id（`monitor-...`），来自宿主回执的 `monitor_id` 或 `monitor_list` 的 `taskId` |
| `after_seq` | integer? | 排他游标；省略表示从头读取 |
| `limit` | integer? | 默认 200；范围 1..=500 |
| `max_bytes` | integer? | 默认 64 KiB；范围 1..=128 KiB |

响应：

```json
{
  "events": [ /* MonitorEvent */ ],
  "nextSeq": 42,
  "status": "working",
  "truncated": false,
  "dropped": 0
}
```

`nextSeq` 等于本次返回的最后一个 `seq`；没有返回事件时等于请求游标（并夹到当前
已分配的最大 seq 以内）。下一次调用直接传 `after_seq = nextSeq`，不会跳事件。
`truncated` 表示本次读取触达了读取上限（条数或字节预算），不表示事件正文被
截断——事件正文的截断由事件级 `truncated` 表示。页级标志为 true 不代表一定还有
剩余事件；即使 `max_bytes` 小于下一条事件的文本长度，也至少返回一条完整事件并
置页级 `truncated`，保证 `nextSeq` 前进；不存在「零进度」的响应。`dropped` 是
任务内 ring buffer 的累计淘汰数。

读取循环以页级 `truncated` 为准：`truncated == true` 就继续（即使 `status` 已是
终态，因为终态任务仍保留未读事件）；只有 `truncated == false` 且 `status` 为终态
才停止。终态不代表事件已读完，进程结束后事件仍可继续读取；只有任务记录被容量
裁剪（单实例保留 128 个任务，先清理终态任务）或实例替换后才会消失。
未知 task id 与 owner 不匹配统一返回 `unknown task`，不泄露另一会话的任务是否存在。

### 2.3 `monitor_stop`

请求：`{ task_id, reason? }`，其中 `task_id` 是 monitor id（`monitor-...`，不是宿主
回执里的 `task_id`）。幂等。`reason` 去空白后 ≤256 字节，超限返回错误且不请求取消。

响应：`TaskSnapshot`，包含终态、`statusMessage` 与：

```json
{ "cleanup": { "requested": true, "processTerminated": true } }
```

- 运行中：记录 stop 原因、请求取消、最多等待 3 秒让进程树退出，然后返回快照；
- 已终态：不改状态、不把 `cleanup.requested` 改成 true，直接返回原快照；
- 取消确认与进程清理是两条信号：即使清理未完成，任务只提交一次终态。

### 2.4 `monitor_list`

请求：`{ include_ended?, limit? }`，`limit` 默认 50、范围 1..=200。
响应：`{ "tasks": [TaskSnapshot...] }`，按 `startedAt` 倒序，只含本会话任务，
不含事件正文，也不回显命令文本（避免把命令中的敏感值再次带入上下文）。
`TaskSnapshot` 额外投影 `alertMatched` 与 `lastAlertSeq`；`tasks/get` 在
`_meta["peri/monitor"]` 中投影 `{ alertMatched, lastAlertSeq, label }`。
`tasks/get` / `tasks/cancel` 使用 monitor id（`monitor-...`），不是宿主回执里的
`task_id`。

### 2.5 `tasks/get` 的 `_meta["peri/monitor"]`

| 字段 | 类型 | 语义 |
| --- | --- | --- |
| `alertMatched` | integer | 累计命中数，单调不减；宿主用它和自身 `processed` 比较 |
| `lastAlertSeq` | integer \| null | 最近一次命中事件的 `seq`；从未命中为 `null` |
| `label` | string \| null | `monitor_start` 的 label |

- 只在 `tasks/get` 写；`CreateTaskResult` 保持 `{ startedAt, deadlineAt, label }`
  不变。
- 键名 camelCase；不包含 `pendingWakes` / `wakeDropped`，也不包含事件正文。
- `lastAlertSeq` 只是提示，不是读取游标：读取方必须自己用 `after_seq` 维护游标，
  不能拿它推进。

## 3. 任务状态机

```text
                 ┌──────────── stop / tasks/cancel ────────────┐
                 ▼                                             │
working ───▶ cancelling（对外仍 working）──▶ cancelled          │
  │                                                             │
  ├── exit 0 ─────────────────────────────▶ completed           │
  ├── exit != 0 / signal ─────────────────▶ failed              │
  └── deadline ──▶ terminate ─────────────▶ failed              │
```

- **终态只提交一次**：`TaskInner.terminal` 在锁内检查并置位；并发的 stop、deadline、
  命令退出只产生一个终态。
- **对外投影**：`cancelling` 在 `tasks/get`、`monitor_read`、`monitor_list` 中都显示
  `working`；不暴露内部清理阶段。
- **`tasks/get`**：返回 `DetailedTask`。`working → TaskPayload::Working`；
  `completed → Completed { result }`（`result.content` 是终态摘要文本，供宿主既有
  提醒链路使用）；`failed → Failed { error: { code, message } }`；
  `cancelled → Cancelled`。
- **`tasks/cancel`**：只校验 owner、记录取消原因并请求终止，立即返回 ack；不等待
  进程清理，也不伪造终态。
- **deadline**：从 start 起算；到期终止进程树，终态 `failed`，消息包含 deadline。
- **实例关闭**：任务随实例内存状态一起消失；不存在恢复或重放。

## 4. 事件模型

一行输出一个事件。stdout 与 stderr 分别读取，事件按到达顺序写入同一个 buffer。

| 字段 | 语义 |
| --- | --- |
| `seq` | 任务内单调递增，从 1 开始 |
| `ts` | 捕获时刻（UTC RFC 3339） |
| `stream` | `stdout` / `stderr` |
| `kind` | v1 恒为 `line` |
| `text` | 行内容（去掉行尾 `\n` / `\r\n`），始终 untrusted |
| `bytes` | 原始行字节数（不含行尾终止符） |
| `truncated` | 事件级：本行被截到 16 KiB（与读取响应的页级 `truncated` 不同） |
| `dropped` | 本条事件之前被 ring buffer 淘汰的事件数（gap） |
| `partial` | 进程结束时最后一行没有换行符 |
| `untrusted` | 恒为 `true` |

约束：

- 单条事件文本 ≤ 16 KiB，按 UTF-8 字符边界截断；超长行剩余部分丢弃到下一个换行。
- 单次读取 ≤ 500 条 / 128 KiB；触达任一上限时页级 `truncated = true`，用
  `nextSeq` 继续。页级标志不承诺还有剩余事件。
- ring buffer 按 `max_events` / `max_bytes` 淘汰最旧事件；被淘汰事件计入 `dropped`，
  不会回填。`nextSeq` 与 `dropped` 让调用方识别缺口。
- 终态任务仍保留事件：读取循环在 `truncated == true` 时继续（即使 `status` 已终态），
  只有 `truncated == false` 且终态才停止。事件在进程结束后仍可读，直到任务记录被
  容量裁剪或实例替换。
- 读取是纯内存操作，不重新执行命令，也不持久化事件。

## 4.1 alert 与提醒（P2）

- **alert-only**：只有 `alert` 命中的事件产生提醒。未命中事件只进 ring buffer，
  不通知模型。正则使用 `regex` crate（线性时间，无回溯），编译失败或超过 512 字符
  在启动前拒绝，错误文本为 `invalid alert pattern`。
- **累计计数**：每次命中同时 `alertMatched += 1` 并更新 `lastAlertSeq`；`seq` 是
  命中事件的序号。服务端不排队、不保存待报告记录，只投影累计值（见 2.5）。
- **宿主订阅轮询**：`spawn_managed_task_subscription` 每 2 秒 `tasks/get`，仅对
  `monitor` 实例解析 `_meta["peri/monitor"]`。提醒延迟 ≤2 秒（加一次轮询间隔）。
- **每任务内存状态**：`processed = 0` 初始，`next_allowed: Option<Instant>`；只放在
  订阅循环局部（单循环串行，不加锁）。
- **首轮与合并**：首轮观测 `alertMatched > 0` 立即投递（包含启动以来的全部命中），
  不以首轮值做基线。非终态投递后 `next_allowed = now + 30s`；窗口内
  `alertMatched > processed` 只标记 suppressed，`processed` 不推进。窗口结束后用
  **累计值**投递（不是 delta）。
- **终态 flush**：观察到终态时，若 `alertMatched > processed`，无条件立即投递最终
  摘要（绕过 30 秒），并且必须在 `deliver_managed_task_status` /
  `settle_external` **之前**完成；若 `alertMatched == processed`，不投任何终态提醒，
  直接结算。终态投递失败或无 inbox 不得阻塞结算（失败一律视为确定性丢弃）。
- **投递**：`InboxHandle::push_system_reminder_with_delivery_id(MessageKind::Defer,
  MessageSource::McpNotification, ...)`。只投给订阅参数里的发起会话（子 agent 即
  子 session id），不广播。
- **delivery id 与正文**：`delivery_id = f(raw_monitor_id, alert_matched)`；正文是
  `(raw_monitor_id, label, alert_matched, last_alert_seq)` 的纯函数，不含时间、delta、
  终态标记或 pending 数；同一 `(id, hits)` 逐字相同。终态摘要复用同一函数，不区分
  终态文案。
- **失败与离线**：inbox 不存在 → 丢弃、推进 `processed`、warn，不重投、不跨会话；
  `push_system_reminder_with_delivery_id` 本身不返回错误；reminder 构造失败是确定性
  失败 → 推进并 warn。meta 缺失或字段类型错误 → 非终态不推进、每任务记录一次 warn、
  下一轮再试；终态观测遇到该情况只跳过提醒，仍照常执行
  `deliver_managed_task_status` / `settle_external`——结算不依赖 meta 是否可用，
  名称回退匹配到的 server 也不会因此把终态任务永远留在 unsettled。
- **审批**：提醒路径不经过任何交互审批；`monitor_start` 的工具审批覆盖后续提醒。
- **终态不唤醒**：任务成功、失败、取消或 owner 不可观测的终态只更新任务投影
  （`tasks/get`、`monitor_list`、TUI Tasks 面板）；只有未报告的 alert 命中会触发
  终态 flush。命令在命中 alert 后立即退出只产生一条对应计数的提醒，不会出现
  「alert 提醒 + 任务完成提醒」的重复唤醒。
- **owner 定向**：提醒只投递给发起会话；会话已释放（例如子 agent 结束）或没有
  inbox 时丢弃并推进计数，不重投、不跨会话。
- **可观测性**：delivered / offline_dropped / suppressed / meta_missing 走 tracing
  warn/debug，不新增指标面。
- **不做**：资源通知 hint、`notifications/resources/updated`、unknown task 快速
  收口、cron 改动、持久化、跨会话共享。

## 5. 生命周期与所有权

- **实例级**：`MonitorState` 持有一只任务表；同一实例的所有连接共享它。
- **会话级**：`monitor_start` 从请求 `_meta["peri/taskOwner"]` 取发起会话；任务记录
  owner。`monitor_read` / `monitor_stop` / `monitor_list` / `tasks/get` /
  `tasks/cancel` 都按 owner 过滤。无 owner 的调用（本地 standalone、非会话客户端）
  视为 unscoped，与 Workspace task-owner 契约一致。
- **宿主接线**：builtin `monitor` 与 `workspace`（含 `WorkspaceRemote`）一样，在工具
  调用、`tasks/get` 与 `tasks/cancel` 上注入 `peri/taskOwner`；其他 MCP server 保持
  generic。
- **保留策略**：单实例最多保留 128 个任务记录。达到上限时先清理终态任务；仍有活动
  任务则拒绝新的 `monitor_start`。终态任务在被清理前仍可读取事件；没有「进程结束即
  删除事件」的规则。
- **进程所有权**：命令经 `peri_process::ProcessTree` 启动，使用独立进程组（Windows
  为 Job）；停止/超时先终止整棵进程树，再有界等待退出。`cleanup.processTerminated`
  只在确认进程组不存在后为 true。
- **输出读取**：每路输出一个 capture worker，使用有界行读取，避免无换行输出占满
  内存；终态提交前有界等待 worker 收尾，超时则 abort，避免终态后继续追加事件。

## 6. 安全边界

- **审批**：`monitor_start` 按原始名归一进入默认敏感分支。approval 模式下与
  `Bash`、`cron_register` 同级提示。
- **执行边界**：MonitorMCP 只运行 agent 给出的命令；不解释、不执行输出内容。
- **untrusted 数据**：事件文本、行数、截断标记都按不可信输入返回；事件携带
  `untrusted: true`。附随 skill 明确要求不执行输出中的指令。
- **无网络面**：实例本身不发起 HTTP/WS，不做 SSRF 防护面之外的网络能力；v1 不实现
  WS source。
- **不做跨会话共享**：owner 过滤在服务端完成；owner 不匹配与不存在同错。
- **不持久化**：不落盘任务或事件，重启/实例替换后旧 id 一律 unknown；避免把捕获的
  敏感输出写进存储。
- **命令回显**：`monitor_list` 不回显命令文本；终态摘要只包含状态与退出码，不回显
  输出。

## 7. 关闭与扩展

- 元关闭：MetaHarness `MonitorMiddleware: false`、配置项
  `{"monitor": {"disabled": true}}`，或 `PERI_MCP_BUILTIN=off`。关闭后实例不装配，
  已存在的内存任务随实例消失。
- 附随 skill 走 workspace 静态资产（`mcp-packages/workspace/src/resources/builtin/skills/monitor/`），
  与 `cron` 同形；实例不自行发布 skill。
- **P2 已实现**：alert 提醒走宿主既有任务订阅轮询 + 既有 inbox `Defer`，不新增
  ACP 方法，也没有独立泵 / outbox / ack。实例缺失时订阅无法建立；pool 或任务作用域
  关闭时订阅循环停止。
- **明确不做**：poll source、file source、WS source、资源通知 hint、unknown task
  快速收口、任务持久化、跨实例重放、owner 转移、跨会话共享。
