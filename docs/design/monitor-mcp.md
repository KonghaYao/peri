# MonitorMCP（command source 监控）

> **状态**：**已批准目标设计**——v1 command source、标准 MCP Tasks 通信与 P2
> alert 唤醒已实现；只有 alert 命中唤醒 owner，任务终态不唤醒。poll/file/WS
> source 不在范围。
>
> **事实源**：`mcp-packages/monitor/src/{server,state,buffer,tools}.rs`（handler、
> 任务状态机、事件 buffer、工具声明）；注册表与工具声明在
> `peri-acp-types/src/builtin_mcp.rs`；宿主分派与 standalone 在
> `peri-middlewares/src/mcp/builtin/{dispatch,standalone}.rs`；owner 元数据与任务
> 终态投影在 `peri-middlewares/src/mcp/{tool_bridge,client/}`。
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
  └──────────── 宿主既有 task 终态投影（轮询 tasks/get → terminal）   MonitorState
                                                                          │
                                        ┌─────────────────────────────────┤
                                        ▼                                 ▼
                                 shell command                      EventBuffer
                                 (stdout / stderr)                  (ring, seq)
                                                                          │ alert match
                                                                          ▼
                                                          WakeOutbox（30s 合并，上限 256）
                                                                          │ monitor_wakes
                                                                          ▼
                                                      宿主 wake 泵（owner 定向 Defer）
```

实例只持有内存状态。宿主或实例被替换后，旧任务 id 不再可解析；没有落盘、恢复或
重放通道。

## 2. 工具契约

四个模型工具全部 deferred（模型面名字 `mcp__monitor__*`），无 prompt 声明模板。
`monitor_wakes` / `monitor_ack` 是 host-only（`model_visible: false`）：保留在实例
`tools/list` 中供宿主 wake 泵调用，不进入桥接、ToolSearch 索引与模型 tools 参数。

### 2.1 `monitor_start`

请求：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `command` | string | 必填，非空 shell 命令 |
| `cwd` | string? | 工作目录；相对路径相对实例 cwd 解析 |
| `label` | string? | 供 `monitor_list` 与 wake 提醒使用的短标签；≤256 字节 |
| `alert` | string? | 线性时间正则，≤512 字符；命中行产生宿主 wake |
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
`TaskSnapshot` 额外投影 `pendingWakes`、`wakeDropped`、`alertMatched`；`tasks/get`
在 `_meta["peri/monitor"]` 中投影同一组计数。`tasks/get` / `tasks/cancel` 使用
monitor id（`monitor-...`），不是宿主回执里的 `task_id`。

### 2.5 `monitor_wakes`（host-only）

请求：`{ limit? }`（默认 50，最大 200）。响应：

```json
{ "wakes": [ { "wakeId", "taskId", "label", "seq", "matched", "createdAt" } ] }
```

只返回当前 owner 的待确认 wake，按任务启动时间排序。正文不含事件文本；owner 不匹配
与不存在同错（返回空列表，不泄露其他会话是否存在 wake）。

### 2.6 `monitor_ack`（host-only）

请求：`{ wake_ids: string[], outcome?: "delivered" | "offline" }`。响应
`{ "acked": n }`。幂等：未知 id 与重复确认都返回 0，不报错；owner 不匹配时同样按
不可见处理。`offline` 表示 owner 无 inbox，wake 被丢弃而不投递。

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

## 4.1 alert 与唤醒（P2）

- **alert-only**：只有 `alert` 命中的事件产生 wake。未命中事件只进 ring buffer，
  不通知模型。正则使用 `regex` crate（线性时间，无回溯），编译失败或超过 512 字符
  在启动前拒绝，错误文本为 `invalid alert pattern`。
- **逐任务 outbox**：wake 记录只有 `wakeId` / `taskId` / `label` / `seq` / `matched`
  / `createdAt`；不含事件原文。`seq` 是并入该 wake 的最新命中事件序号。
- **30 秒合并**：最新 pending wake 在 30 秒窗口内接收后续命中，只累加 `matched`
  并更新 `seq`。窗口外的命中开启新 wake；宿主确认后窗口内的命中先留在 draft，
  到期或任务终态时再生成一条。该规则保证「每任务最多一条 wake / 30 秒」，即使宿主
  每次都立即确认也不会形成风暴。
- **容量**：每任务最多 256 条 pending wake；超出淘汰最旧一条并累加 `wakeDropped`。
- **投递**：宿主 wake 泵每 0.5–1 秒按 owner 调用 `monitor_wakes`（带
  `_meta["peri/taskOwner"]`）。有 inbox 时构造 `Defer` 提醒（只含 raw task id、
  label、seq、matched）并 push；随后 `monitor_ack outcome=delivered`。无 inbox 时
  不投递，直接 `monitor_ack outcome=offline` 并计数。
- **幂等**：`delivery_id = f(task_id, wake_id)`（`peri-acp-types::monitor`）。
  ack 失败时下一轮重试，重复投递复用同一 canonical id。
- **审批**：wake 路径不经过任何交互审批；`monitor_start` 的工具审批覆盖后续 wake。
- **终态不唤醒**：任务成功、失败、取消或 owner 不可观测的终态只更新任务投影
  （`tasks/get`、`monitor_list`、TUI Tasks 面板），不向 owner inbox 投递
  reminder。只有 alert 命中产生 `Defer` wake；因此命令在命中 alert 后立即退出也
  不会出现「alert wake + 终态 reminder」的重复唤醒。
- **owner 定向与离线**：wake 只投递给发起会话；owner 已释放（例如子 agent 结束）
  或没有 inbox 时丢弃并计数，不重投、不跨会话。泵只跟踪启动过 monitor task 的
  owner，并在 binding 释放后的短暂宽限期（用于把在途 wake 以 `offline` 确认）后
  移除该 owner，集合不会随会话数无界增长。
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
- **P2 已实现**：alert 与 owner 定向 wake 走宿主轮询 + 既有 inbox `Defer`，
  不新增 ACP 方法。wake 泵与 monitor 实例同池；实例缺失时泵保持 no-op，pool
  关闭（或任务作用域关闭）时停止。
- **明确不做**：poll source、file source、WS source、资源通知 hint、unknown task
  快速收口、任务持久化、跨实例重放、owner 转移、跨会话共享。
