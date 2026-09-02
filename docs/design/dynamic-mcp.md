# Dynamic MCP：Agent 运行时加载设计 · 定稿

> 本文件是 Perihelion Dynamic MCP 的权威设计，回答一个问题：**Agent 如何在运行中的 session 内，经 HITL 动态创建、观察和卸载 MCP server，并在不破坏工具可见性、任务所有权和关闭契约的前提下使用其能力。**
>
> 状态：**现行设计**；具体缺陷与修复进度由 `spec/issues/` 中对应 active issue 维护
>
> 关联事实源：`docs/standards/architecture-contracts.md`、`docs/reference/mcp-ecosystem.md`、`docs/standards/testing.md`、`peri-middlewares/CLAUDE.md`。若本文与代码或跨模块架构契约冲突，以代码、契约测试和 standards 为准。

## 1. 目标与范围

### 1.1 目标

Dynamic MCP 允许 Agent 在 ReAct loop 运行期间提交 MCP server 配置，经用户批准后异步建立连接，并让同一 session 内的 Agent 在后续 Reason 阶段发现和调用新工具。

必须同时满足：

1. **session 隔离**：动态能力只属于发起 session，不改变其他 session 的工具视图。
2. **显式授权**：建立连接或启动进程本身必经 HITL；批准加载不等于批准后续工具调用。
3. **异步可观察**：慢启动、OAuth、连接和发现不阻塞单次工具调用；状态可推送、可查询。
4. **目录一致性**：工具目录只在 Reason 边界原子更新，不在正在执行的模型请求或工具调用中途漂移。
5. **单一任务所有权**：initialize、OAuth、reconnect、subscription 和关闭任务全部纳入 deployment-held `McpTaskOwner`。
6. **可撤销**：卸载先撤销新调用能力，再优雅排空在途调用和连接。
7. **秘密隔离**：模型只可提交 secret 引用；平台控制的 tool arguments、审批、operation、通知、日志和错误不得包含解析后的 secret。MCP server 是被授权的 secret recipient，平台不承诺阻止不可信 server 在工具结果中主动回显或变形泄露其已获得的 secret。

### 1.2 在范围内

- Agent-facing `DynamicMCP` deferred tool。
- `load`、`status`、`unload` 三个 method。
- stdio 与 Streamable HTTP 动态配置。
- session capability overlay、静态 server 遮蔽和恢复。
- session 内主 Agent 与 SubAgent 的能力共享。
- 异步 operation 状态机和进度投影。
- MCP tools、resources、skills 的运行时发现与移除。
- ToolSearch session-local 索引刷新。
- HITL effective action、脱敏审批卡和 secret 引用解析。
- session close 与 host shutdown 清理。

### 1.3 不在范围内

- 将动态配置写入 `~/.peri/settings.json`、项目 `.mcp.json` 或 plugin manifest。
- session 恢复或进程重启后的动态 server 恢复、重连或失效提示。
- `replace`、显式 `reconnect`、同名多实例。
- MCP Apps 透传与 view 渲染；该主题仍由 `mcp-multiplexing.md` 负责。
- 修改 `DiscoverMCP` 的只读语义。
- 为动态 MCP 设计独立于现有 MCP tool HITL 的调用授权体系。

## 2. 代码现状与设计约束

当前生产链为：

```text
ACP host
  → 创建 host 级 McpClientPool + deployment-held McpTaskOwner
  → run_initialize() 合并 global / plugin / project 配置
  → McpMiddleware::collect_tools() 构造 McpToolBridge
  → session-local shared_tools
  → ToolSearch session-local index
  → Reason 暴露 direct tools / 搜索 deferred tools
  → tool_dispatch 执行 effective tool
```

设计受以下事实约束：

| 事实 | 设计含义 |
| --- | --- |
| `McpClientPool` 当前以 `server_name` 为 key，且由 host 共享 | 动态 server 不能直接按用户可见名称写入现有全局 namespace，否则跨 session 串扰 |
| `McpToolBridge` 持有固定 `Arc<McpClientHandle>` | pool 新增 client 不会自动更新已装配 session 的工具目录 |
| Agent dispatch 从 session-local `shared_tools` 读取 | 动态加载必须更新 session-local capability 和工具快照 |
| ToolSearch 从 session-local 工具视图构建 deferred 索引 | MCP bridge 与 ToolSearch 索引必须在同一 generation 提交 |
| frozen prompt 在 session 创建时冻结 | 动态 tools 可变，但不得重建 frozen system prompt 或 frozen skills summary |
| MCP 远端 skills 注册表已经是 session 级 | 动态 server 的 skills 投影复用 session registry，不能写入全局技能缓存 |
| HITL 按解析后的 effective tool name 授权 | `ExecuteExtraTool` 包装不得把 `DynamicMCP.load/unload` 降级成普通 wrapper 审批 |
| pool 关闭有固定顺序 | 动态任务不能裸 `tokio::spawn`，也不能在 host shutdown 后注册新任务 |

## 3. 核心决策

### 3.1 连接所有权与能力可见性分离

物理连接仍由 host deployment 管理，逻辑可见性由 session capability view 管理：

```text
Host MCP deployment
  ├── static connections: global / plugin / project
  ├── dynamic connections keyed by (session_id, server_name)
  └── McpTaskOwner

SessionMcpCapabilityView
  ├── static baseline
  ├── dynamic overlay
  ├── shadow resolution
  └── generation
```

逻辑名称与物理实例必须分离：

```rust
struct DynamicMcpLogicalKey {
    session_id: SessionId,
    server_name: String,
}

struct DynamicMcpInstanceKey {
    logical: DynamicMcpLogicalKey,
    incarnation_id: DynamicMcpIncarnationId,
}
```

`incarnation_id` 是不可复用的 opaque identity。generation 只表示 capability catalog 版本，不能代替 incarnation，也不能单独防御 ABA。所有 handle 发布、operation transition、OAuth callback、通知、resource/skill/command 投影和 cleanup 都必须携带并校验 instance key。

用户可见名称和工具名前缀保持 `server_name`，例如 `mcp__github__search`。内部禁止仅以该名称定位动态连接。

### 3.2 Session 共享规则

动态 MCP 是 session capability，不是 agent-local capability：

- 主 Agent 加载后，同 session 的现有和后续 SubAgent 可见。
- SubAgent 加载后，主 Agent及其他同 session Agent 可见。
- 各 Agent 独立记录已应用的 capability generation，并在自己的下一次 Reason 前刷新。
- 其他 session 不可观察该动态 server、operation、skills、resources 或 tools。

### 3.3 动态层遮蔽静态层

解析同名 server 时，优先级为：

```text
session dynamic overlay > merged static baseline
```

动态 `github` ready 后，当前 session 中静态 `github` 的工具、资源和 skills 被遮蔽；其他 session 不受影响。动态层卸载完成后恢复静态层。

遮蔽必须以 server identity 为单位，不可混合两层的工具清单。动态层尚未 ready 时静态层继续有效；只有动态连接完成发现并原子提交 capability 后才切换。

## 4. Agent 工具契约

### 4.1 `DynamicMCP`

`DynamicMCP` 是 deferred tool，不修改 `DiscoverMCP`。其 schema 采用统一的 `method + params`：

```json
{
  "type": "object",
  "properties": {
    "method": {
      "type": "string",
      "enum": ["load", "status", "unload"]
    },
    "params": { "type": "object" }
  },
  "required": ["method"]
}
```

通过 `SearchExtraTools → ExecuteExtraTool` 调用时，HITL 必须投影最终 effective action：

```text
DynamicMCP.load
DynamicMCP.unload
```

`status` 是只读操作，不扩大或撤销能力，可按现有只读工具策略处理。

### 4.1.1 Canonical effective invocation

现有 `ExecuteExtraTool` 只把 wrapper 解析为目标工具名；仅注册一个 `DynamicMCP` 工具不足以形成 method 级审批。Dynamic MCP 必须在 canonical tool invocation resolver seam 增加第二层解析：

```text
ExecuteExtraTool(raw)
  → target = DynamicMCP
  → 严格解析并规范化 method/params
  → policy_call.name = DynamicMCP.load | DynamicMCP.status | DynamicMCP.unload
  → policy_call.input = 脱敏 canonical input
  → HITL / permission policy
  → 执行已解析的 immutable canonical action
```

`invoke` 不得在审批后重新解释可变 raw input。未知 method、重复字段、类型错误和无法脱敏的配置必须在 HITL 前失败且零副作用。测试必须断言 broker 实际观察到 method 级 effective name，不能只断言出现过一次审批。

### 4.2 `load`

请求示例：

```json
{
  "method": "load",
  "params": {
    "name": "github",
    "config": {
      "command": "github-mcp-server",
      "args": ["stdio"],
      "env": {
        "GITHUB_TOKEN": { "secretRef": "github-token" }
      }
    }
  }
}
```

HITL 通过后立即创建异步 operation 并返回：

```json
{
  "operationId": "mcpop_opaque",
  "server": "github",
  "state": "starting",
  "scope": "session",
  "idempotent": false
}
```

返回 `starting` 只表示任务已被 `McpTaskOwner` 接纳，不表示连接或工具发现成功。

### 4.3 `status`

传入 `operationId` 或 `name` 时返回对应状态；无参数时返回当前 session 的动态 server 与 operation 列表。不得暴露其他 session 的存在。

状态结果包含：operation ID、server name、state、当前阶段、脱敏配置摘要、稳定错误码、安全错误摘要、工具/资源数量和 capability generation。不得返回 secret 值、底层错误链或 server stdout/stderr 原文。

### 4.4 `unload`

请求：

```json
{
  "method": "unload",
  "params": { "name": "github" }
}
```

`unload` 必经 HITL。接纳后返回独立 operation；`draining` 不等于已经关闭：

```json
{
  "operationId": "mcpop_opaque",
  "server": "github",
  "state": "draining"
}
```

## 5. 配置、秘密与审批

### 5.1 配置来源

产品不限制配置来源。Agent 可提交完整 stdio 或 HTTP MCP 配置，不要求引用预注册 catalog。

“不限制来源”不取消以下结构约束：

- stdio 必须以 executable + argv 直接启动，禁止 shell 字符串求值。
- 配置必须恰好选择一种 transport。
- URL、headers、cwd、timeout、protocol version 等字段必须类型化解析。
- 配置字段不得通过模板间接把 secret 展开进审批文本。
- 运行时路径与网络访问遵循宿主现有权限和平台限制；本设计不承诺额外 sandbox。

### 5.2 Secret 引用

动态配置中的敏感值只能表示为 opaque secret reference。Agent tool schema 不接受明文 secret 字段。secret resolver 在 HITL 批准后、启动 transport 前解析引用；解析结果只存在于最小执行作用域。

必须保证：

- config hash 基于规范化配置和 secret reference identity，不包含 secret value。
- Debug、Serialize、tracing 和错误类型不能携带解析后的 secret。
- 连接失败摘要先脱敏再进入 operation、通知或 UI。
- server env 必须先 `env_clear`，再显式加入运行所需的最小非秘密环境和已批准 secret；禁止默认继承整个宿主环境。
- secret 可由 transport 或 stdio child 在连接生命周期内持有，但不得进入长期可序列化控制面状态。
- secret 缺失进入 `failed(SECRET_NOT_FOUND)`，不得要求 Agent 在下一次 tool call 中提交明文。

MCP server/process 是 secret 的最终接收者，属于审批时必须展示的信任边界。平台可以保证自身生成的控制面数据不包含 resolved secret，但无法保证恶意 server 不在 tool result 中回显、编码或变形泄露 secret；如未来要求结果防泄漏，必须另行设计 taint/redaction 机制，且不得宣称可完全阻止变形泄漏。

### 5.3 HITL 审批卡

`load` 审批展示完整脱敏配置：

- server name 与 session scope；
- transport；
- executable/argv 或 URL；
- cwd；
- env 变量名到 secretRef 名称的映射；
- headers 中非敏感字段与 secretRef；
- timeout、protocol version；
- 是否遮蔽静态 server及其来源。

审批前不得启动进程、联网、探测 server、触发 OAuth 或解析 secret value。拒绝审批不得创建 operation、写 capability view 或产生后台任务。

## 6. Operation 与 Server 状态机

### 6.1 Load 状态机

```text
HITL approved
  → Starting
  → Authorizing?       (需要 OAuth 时)
  → Connecting
  → Discovering
  → Ready

任一非终态
  → Failed(code, safe_summary)
```

`Ready` 的提交条件是：

1. transport 已建立且 MCP initialize 完成；
2. server handle 已发布到 dynamic deployment registry；
3. tools/resources 基线发现完成；
4. session capability overlay 已原子提交；
5. generation 已递增；
6. 进度事件已具备可查询的持久内存状态。

skills 发现可以在 Ready 后异步继续，但必须沿现有 session skill registry 投影；其完成不再次改变 MCP tool generation，除非工具或资源目录本身发生变化。

### 6.2 Unload 状态机

```text
Ready | Failed
  → Revoking
  → Draining
  → Unloaded

Draining timeout / cleanup failure
  → Failed(SHUTDOWN_INCOMPLETE)
```

顺序不可交换。卸载的线性化点必须在同一 entry lock/CAS 下原子完成前两步：

1. admission `Open → Draining`，使任何旧 bridge 都无法取得新的 in-flight permit；
2. 撤销 session capability overlay 并递增 generation；
3. 下一 Reason 恢复静态层或移除该 server 工具；
4. 等待已取得 RAII permit、已进入远端调用的请求完成；
5. 停止 subscription、OAuth 和 reconnect admission；
6. 由 owner 管理的任务终止并 join；
7. 关闭该 incarnation 自己持有的 rmcp service/transport；
8. 发布 `Unloaded`。

旧 cleanup 只能关闭它持有的 incarnation service object，禁止按裸 name 回查并关闭当前实例。排空超时后 gate 保持 Draining，不得重新 Open。清理超时不得发布 `Unloaded`。失败 entry 保留到再次 unload 成功或 session 结束。

### 6.3 稳定错误码

首版至少定义：

```text
INVALID_CONFIG
SECRET_NOT_FOUND
CONFIG_CONFLICT
START_REJECTED
CONNECT_TIMEOUT
AUTH_REQUIRED
AUTH_FAILED
INITIALIZE_FAILED
TOOL_DISCOVERY_FAILED
TOOL_NAME_CONFLICT
TASK_OWNER_CLOSED
NOT_FOUND
SERVER_BUSY
SHUTDOWN_INCOMPLETE
INTERNAL
```

错误面只返回稳定 code、阶段和固定安全摘要。原始 SDK、process 或网络错误不得直接投影。

## 7. 幂等、并发与发布原子性

### 7.1 配置 identity

规范化配置 identity 必须覆盖所有影响连接行为的非秘密字段以及 secret reference identity，包括：transport、command、args、cwd、env key/ref、URL、headers key/ref、protocol version、subscriptions 与 timeout。实现可使用碰撞安全 digest 加速索引，但幂等判定最终必须比较 canonical redacted config 结构，不能只比较现有 `u64` hash。

同一 `(session_id, server_name)`：

- 相同 hash 的并发或重试 `load` 返回同一 active operation/server，`idempotent=true`。
- 不同 hash 返回 `CONFIG_CONFLICT`，不自动 replace。
- `Draining` 时拒绝新 load；必须等待 terminal state。
- `Failed` 且 hash 相同时也返回现有失败 operation；首版没有显式 reconnect，Agent 应 unload 后重新 load。

幂等检查、operation reservation 和 task admission 必须形成明确协议，防止重复 stdio process、OAuth flow 或无 owner 的悬挂 operation：

1. registry lock 下验证或创建带 incarnation token 的 reservation；
2. 使用 `task_kind + session_id + incarnation_id` 同步申请 task admission；
3. admission 成功才发布 `Starting`，admission 失败则在同一受控路径发布 terminal `Failed(TASK_OWNER_CLOSED)`；
4. task 开始和每次提交前重新校验 reservation token。

operation 不得停留在没有 owner task 的非终态。task admission 错误应区分 owner closed 与 duplicate key，不能把所有失败静默折叠。

### 7.2 两阶段发布

连接过程使用未发布 entry；失败前不得污染 session capability：

```text
prepare: validate → approve → resolve secret → connect → discover
commit:  install handle → install overlay → generation++
```

commit 必须原子决定动态层和静态层谁可见。不能出现动态 tools 与静态 tools 混合的中间快照。提交必须执行 incarnation-aware CAS，并同时验证：

```text
session open
&& logical entry reservation == this incarnation
&& operation still active
&& admission == Open
&& host deployment open
```

任何检查失败都不得发布 Ready；staged transport 必须由其 RAII owner 收口。所有迟到的 discovery、OAuth、subscription、notification、skill/command commit 和 cleanup 都必须做相同的 instance 校验。

### 7.3 Session close 竞态

session close 首先关闭 dynamic admission，再撤销全部 capability，最后排空该 session 的动态任务和连接。close 与 load 并发时只允许两种结果：

- load 在线性化点前被拒绝为 `TASK_OWNER_CLOSED`；或
- load 已被接纳，但随 session close 被 owner 收口且永不发布 Ready。

禁止 session 删除后迟到任务重新写入 capability view、skills registry、command registry或通知目标。

## 8. 工具目录热更新

### 8.1 Generation 模型

每个 `SessionMcpCapabilityView` 持有单调递增 `generation`。每个 Agent runtime 持有 `applied_generation`。

```text
load commit / unload revoke / relevant list_changed
  → capability generation++
  → Agent 下一次 Reason 前检测差异
  → 构造新的 MCP tool/resource/skill 投影
  → 原子提交 MCP tools + ToolSearch index
  → applied_generation = generation
```

进度通知不是工具目录事实源。即使通知丢失，Reason 仍通过 generation 检测完成刷新。

### 8.2 不可变 Catalog Snapshot

工具目录的原子单位不是若干共享 map 上相同的 generation 数字，而是单个不可变对象：

```text
SessionToolCatalogSnapshot
  ├── generation
  ├── canonical tool map
  ├── direct definitions
  ├── deferred search index
  ├── alias/source metadata
  └── dynamic incarnation admission handles
```

刷新先离线构建完整 snapshot，成功后通过一次 `Arc` swap 发布。Reason 获取 snapshot 后，由该次 Reason 产生的 tool calls 必须固定使用同一 snapshot dispatch；Act 阶段不得按 live map 将旧名称重新解析到新 incarnation。unload 的 admission gate 仍可即时拒绝旧 snapshot 的新调用。

刷新只替换 MCP 来源的工具，不得覆盖 filesystem、terminal、workflow、skills、subagent 或其他 middleware tools。实现应记录工具来源，不依靠 `mcp__` 名称前缀删除条目。

MCP bridge map 和 ToolSearch deferred index 必须来自同一个 capability snapshot，并以同一 generation 发布。禁止出现：

- SearchExtraTools 能发现但 ExecuteExtraTool 找不到；
- ExecuteExtraTool 可调用但搜索索引尚不可见；
- 同一 server 的静态和动态 bridge 混合。

如果 index 重建失败，则不提交任何一半，保留旧 generation 并在下一 Reason 重试；不得让 capability commit 回滚物理连接。

### 8.3 调用 admission

从工具表移除 bridge 不足以阻止已克隆的旧 `Arc<dyn BaseTool>`。每个动态 server entry 必须具有 admission gate：

```text
Open → Draining → Closed
```

`McpToolBridge::invoke` 使用 RAII permit 原子检查 gate 并登记 in-flight：

- `Open`：取得 permit 后调用；
- `Draining/Closed`：返回稳定的不可用错误；
- 已取得 permit 的 in-flight 调用允许完成，permit Drop 时减少计数并唤醒 drain waiter。

卸载不得仅依赖 `Arc` 引用计数判断是否安全关闭。

## 9. 进度、通知与发现

### 9.1 双通道可观察性

状态有两个投影：

1. `DynamicMCP.status`：权威的 session-local 内存状态查询。
2. 现有 MCP status/inbox 流程：向 Agent 推送阶段变化，促使后续 Reason 发生。

通知可能合并、延迟或丢失，不能作为状态机存储、幂等依据或目录刷新触发的唯一来源。动态状态禁止复用 host-global `pending_changes` 或 pool singleton notifier；必须投递到 checked session-specific inbox，并携带 session/instance identity。session close 后发送失败即丢弃，不得降级广播。通知文本不得含 secret、完整 URL query、Authorization header 或 process 原始输出。

### 9.2 Resources 与 Skills

动态 Ready 后：

- resources 通过该 session capability view 被 `DiscoverMCP` 与 `mcp_read_resource` 观察；
- skills 按现有 session `McpSkillRegistry` 异步发现；
- MCP skill 不进入 frozen summary 或 system prompt；
- unload/revoke 必须立即移除该动态 server 的 skill 与 command 投影；
- 迟到的发现任务必须以 `(session_id, incarnation_id, handle token)` 校验后才可提交，防止卸载或重载后的 ABA 污染。

skills/commands 可使用独立 projection generation，但必须与 MCP instance incarnation 绑定；catalog generation 不得替代该校验。

### 9.3 OAuth、缓存与名称空间隔离

动态 OAuth 的 callback、cancel、complete、failure 和事件路由必须以 opaque flow token 定位；identity 至少绑定 `(session_id, incarnation_id, flow_id)`。动态路径禁止调用只按 `server_name` 定位的 callback API。session close/unload 必须撤销该 incarnation 的 sender 和 active flow reservation。

首版动态 MCP 禁用跨 operation、incarnation 或 session 的 persistent resource cache。若使用内存缓存，其 key 至少包含 `session_id + incarnation_id + normalized config identity + resource identity`，并在 unload/session close 时清除。静态 pool 中仅按 `server_name` 定位的 cache helper 不得用于动态连接；resource list、cache version、skill digest 适用同样规则。

### 9.4 名称规范与碰撞

现有“非法字符替换为 `_`”的 sanitizer 非单射，Dynamic MCP 不得直接依赖它静默注册工具。实现必须选择并固化一种策略：对动态 server/tool 名采用无变换允许字符集并拒绝非法名称，或使用可逆单射编码。

commit 前必须对最终 canonical tool name、ASCII case-folded name 和 alias 做完整 catalog 碰撞检测，包括同 server 内工具、静态 MCP、其他动态 MCP 和内置工具。任何碰撞使 load 以稳定错误 `TOOL_NAME_CONFLICT` 失败，不得由 `BTreeMap::insert` 静默覆盖。HITL policy identity 还必须绑定 source/incarnation identity，不能只依赖 display name。

## 10. 生命周期与关闭

### 10.1 任务所有权

所有动态 MCP 后台工作必须通过现有 `McpTaskOwner`/spawner 注册，包括：

- initialize/connect；
- OAuth；
- reconnect；
- subscription；
- skill discovery；
- unload drain/cleanup。

禁止独立 `tokio::spawn` 持有 pool 或 session registry 强引用。任务 key 必须包含 `task_kind + session_id + incarnation_id`，避免与静态 server、其他 session 同名任务及同 server 的其他任务冲突。unload orchestrator 不得停止包含自身的 task group；owner API 必须支持按 kind 有序停止，或排除当前 orchestrator token。

### 10.2 Session close

session close 自动触发所有动态 entry 的 revoke 和 cleanup，不要求 HITL。关闭属于已授权 session 的资源回收，不是 Agent 主动卸载。

### 10.3 Host shutdown

继续遵守全局关闭契约：

```text
pool / dynamic registry begin-close
  → McpTaskOwner abort + join
  → pool-owned service close transaction
```

cleanup timeout 保持 Closing/Incomplete，不得声明 Closed。动态 entry 必须被计入同一 terminal shutdown evidence，不能另建不可观察的关闭路径。

### 10.4 Staged resource RAII

单 server connect/discover 必须返回 staged RAII owner；在 transport 发布前，它独占 stdio child、HTTP service、OAuth listener/callback 和相关取消令牌。timeout、cancel、discovery failure、commit CAS 失败、session close 和 host begin-close 都必须触发 close/kill/reap。stdio child 需要明确的 kill-on-drop 与 wait/reap 契约，不能假设 transport Drop 足够。

只有 staged resource 已清理后，load 才能进入普通 `Failed`。若 cleanup 不完整，operation 必须进入可观察的 `SHUTDOWN_INCOMPLETE`，不能用普通业务失败掩盖遗留 process/transport。

### 10.5 不持久化

动态配置、operation、授权结果和 capability overlay 都不写入持久 session。进程退出后完全丢弃；恢复 session 时不自动重连，也不插入能力失效提示。历史 transcript 中过去的 MCP 调用保持原样，但当前工具目录仅以当前 runtime capability 为准。

## 11. 跨模块落点

| 责任 | 规范落点 |
| --- | --- |
| `DynamicMcpDeploymentPort`：按 session 执行 load/status/unload，不暴露具体 registry | `peri-acp-types` 定义，`peri-middlewares` 实现，ACP host 注入 |
| `SessionMcpCapabilityPort`：提供 immutable catalog source/generation | `peri-acp-types` 定义，Agent session 持有端口 |
| `SessionCloseRegistration`：幂等 revoke session lease | `peri-acp-types` 定义，Agent session close 调用 deployment port |
| operation DTO、稳定错误码、secret reference、task port | `peri-acp-types` |
| 单 server connect/discover、dynamic registry、incarnation、admission、drain | `peri-middlewares/src/mcp/` |
| `DynamicMCP` tool 与 canonical effective invocation | `peri-middlewares` 工具 resolver 与 Agent canonical dispatch seam |
| session capability 注入与 middleware 装配 | `peri-agent/src/session/factory.rs`、`peri-middlewares/src/assembly.rs` |
| Reason snapshot 获取与 pinned dispatch | `peri-agent/src/agent/stages/reason.rs`、`tool_dispatch.rs` 及 session-local catalog |
| ToolSearch immutable index 构建 | `peri-middlewares/src/tool_search/` 与对应 contract port |
| session close / host shutdown task owner | `peri-agent` session runtime 与 `peri-acp` host deployment |
| 动态状态通知 | checked session inbox/ACP 事件投影，不复用 host-global MCP change buffer |

ACP 只负责 deployment 装配、端口注入、session 定位和协议化投影，不持有或解释 Dynamic MCP 业务状态机。`McpTaskOwner` 仍是唯一 deployment-held non-Clone owner；dynamic registry 只持 weak spawner，不得创建 per-session owner。registry 不得强持有 Agent Session、inbox、skill 或 command registry，只持 checked weak sink/session lease。连接、工具构造和生命周期执行属于 Agent/middleware 侧，并通过契约端口维持依赖方向。

## 12. 验收标准

### 12.1 P0 契约测试

1. `DynamicMCP.load/unload` 经 `ExecuteExtraTool` 时，broker 观察到的 effective name 严格为 `DynamicMCP.load` / `DynamicMCP.unload`；`status` 的只读策略不能批准同一 wrapper 下的 mutate method，拒绝后零副作用。
2. 审批文本、operation、通知、tracing 和平台生成错误不包含 secret value；文档和审批明确 MCP server 可主动回显其已获 secret 的信任边界。
3. 两个 session 可加载同名不同配置 server，tool/resource/cache/OAuth callback/通知与状态互不可见。
4. 同 session 同名同 canonical config 并发 load 只启动一个 process/connection；不同配置返回 `CONFIG_CONFLICT`。
5. `a.b`/`a_b` 等 server/tool 名称不会经 sanitizer 静默碰撞；任何 catalog collision 均使 load 失败。
6. load 失败不遮蔽静态 server，也不发布半成品 tools/resources/skills；staged stdio/HTTP/OAuth resource 已关闭。
7. Ready 后工具只在下一 Reason 生效；一次 `Arc` swap 同时发布 bridge、direct definitions 与 ToolSearch index。
8. Reason 在 generation N 取得 snapshot 后，即使 N+1 已发布，其 dispatch 仍固定解析到 N 的 target，不会路由到同名新 incarnation。
9. unload 线性化点后旧 bridge 无法取得新 permit，已取得 permit 的 in-flight 可完成；关闭未完成不得报告 Unloaded。
10. load L1 → unload → load L2 后，L1 的迟到 commit/cleanup/OAuth/通知不能修改或关闭 L2。
11. 动态层卸载后同名静态层恢复，且不混合两层工具。
12. 已运行 SubAgent 跨越 load/unload 后在下一 Reason 应用同一 session capability，同时保留自身 allowlist/disallowlist。
13. session close 后迟到 capability、skill、command、OAuth callback、cache 或通知写入全部被拒绝。
14. host shutdown 遵守 begin-close → owner abort/join → service close，并把动态 task/service 纳入统一 shutdown evidence。

### 12.2 P1 状态机与故障测试

- stdio / HTTP connect success、timeout、initialize failure、tool discovery failure。
- secretRef missing、resolver failure、脱敏边界。
- OAuth pending、拒绝、成功、session close 竞态。
- notification 丢失后 status 与 generation refresh 仍正确。
- ToolSearch index rebuild 失败保持旧快照并可重试。
- unload draining timeout、subscription/reconnect 停止、ABA handle token 防护。
- 主 Agent与现有/后续 SubAgent双向共享；其他 session 隔离。
- failed operation 保留到 unload 或 session close。
- session 恢复后动态状态为空且不自动重连。

### 12.3 建议验证命令

实施后至少运行：

```bash
cargo test -p peri-acp-types --lib
cargo test -p peri-middlewares --lib -- mcp
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
```

## 13. 明确拒绝的替代方案

| 方案 | 拒绝原因 |
| --- | --- |
| 直接把动态 client 以 `server_name` 写入 host 共享 pool | 跨 session 名称冲突和能力泄露 |
| 动态连接完成后立即原地修改任意 Agent 的工具 map | 破坏 Reason/dispatch 快照一致性，收益有限 |
| 扩展 `DiscoverMCP` 承担 load/unload | 破坏现有只读发现契约 |
| load 审批同时授权全部 MCP tools | server 工具列表和行为可变化，授权范围失真 |
| 允许明文 env/header secret 出现在 tool arguments | secret 会进入模型内容、transcript 与潜在遥测 |
| unload 直接取消所有在途工具 | 外部副作用可能已发生，取消会制造未知结果 |
| 每个 session 建立完整静态 MCP pool | 重复静态连接，破坏现有 host 级复用与关闭模型 |
| 将动态配置自动写入 settings 或 session store | 违反本设计的进程内临时能力语义 |
