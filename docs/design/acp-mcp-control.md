# ACP MCP 运行控制

> 状态：现行设计。取代旧 Dynamic MCP 设计与能力。
>
> 本文是 MCP 运行控制的权威契约。现行行为以代码和契约测试证明，代码入口见相关 code-index。跨层规则仍遵循 `docs/standards/architecture-contracts.md`。

## 1. 目标与边界

1. 外部客户端经 ACP 动态注册、重启、关闭 MCP，不要求模型工具调用或 HITL。
2. TUI MCP 面板本轮提供重启和关闭；不提供动态配置编辑器。
3. 删除旧 `DynamicMCP` Agent 工具、load/status/unload 协议、动态覆盖层、独立 operation/secret resolver、专属 OAuth 身份及装配。不保留旧工具或控制协议的兼容入口、别名或双写。
4. 使用当前 session 环境已有 MCP pool 和唯一任务 owner；不再建立另一套 deployment registry。
5. 运行控制的配置与运行状态不写配置文件、session store 或数据库，不改变表结构；已有 OAuth credential store 的正常凭据保存保持原契约。进程重启和冷加载不会恢复临时注册或关闭状态。
6. MCP over ACP 的 `mcp/message` 与 Apps 协议继续保持各自边界。

## 2. 身份与作用域

所有请求必须显式携带 `sessionId`。宿主定位已建立、开放的 SessionEnvironment，使用其 pool，禁止 fallback 到启动环境或另一个 session。

控制的实际作用域是该 session 的 MCP 环境：共享该环境的主/子 Agent 共同观察变化；独立环境互不影响。`serverName` 是该 pool 的稳定名称，不是全宿主身份。

注册采用独立名称，不覆盖已有静态、插件、ACP 或临时条目。新注册名称采用 `[A-Za-z0-9_-]+`，拒绝 builtin 保留名与规范化/ASCII case-fold 后会碰撞的名字。restart/stop 精确匹配目录中的 opaque 稳定名称，允许既有插件名称中的冒号。注册使用专用严格 stdio/HTTP DTO（显式 `type`），校验 command/url 排他、非空 executable、HTTP(S) URL、字段类型；转换到内部配置时来源由宿主赋值，不接纳 builtin、WorkspaceRemote、ACP 来源身份及 system MCP 提权字段。

本版重启/关闭仅针对可管理的普通 stdio/HTTP MCP。builtin、WorkspaceRemote、system MCP 和 MCP-over-ACP 明确返回 unsupported，避免切断核心工具与任务环境；后续扩展须单独修订契约。

## 3. ACP wire

新增一个 JSON-RPC 方法 `peri/mcp/control`，客户端 initialize 声明 `_meta["peri.mcpControlV1"] = true`，服务端声明已实现支持。未 initialize、未协商与重连后默认不支持，不能使用 legacy all_enabled 默认值。必须按当前连接能力门控；能力协商不是认证，网络部署仍负责连接和 session 访问授权。

三种请求使用严格 tagged DTO，camelCase，拒绝未知字段、缺失字段和 action/config 不匹配：

```json
{"sessionId":"s1","action":"register","serverName":"docs","config":{"type":"http","url":"https://example.com/mcp"}}
```

stdio 注册使用同一 action，`args`、`env` 缺省为空；执行目录由已有会话环境提供：

```json
{"sessionId":"s1","action":"register","serverName":"localdocs","config":{"type":"stdio","command":"docs-mcp","args":["--stdio"],"env":{}}}
```

```json
{"sessionId":"s1","action":"restart","serverName":"docs"}
```

```json
{"sessionId":"s1","action":"stop","serverName":"docs"}
```

响应只在本次动作达到可证明的结果后成功：

```json
{"sessionId":"s1","serverName":"docs","action":"register","status":"connected"}
```

`status` 为 `connected` / `stopped` / `needsAuthorization`。后者只表示已记录需要 OAuth，不表示可以调用工具；控制请求不启动、不等待交互 OAuth；授权由客户端经既有 OAuth start/cancel 流程显式发起。移除旧动态身份后，OAuth 以 session 选池与普通 flow ID 定位，不能依赖已删除的 incarnation 协议。

慢控制请求由 host-owned task 处理，不阻塞 server loop 的 prompt、cancel、反向交互及其他 session。传输取消不转移 cleanup 所有权；连接终止由已有 host/pool shutdown 收口。调用方未收到结果时可用现有 `mcp/list` 查询实际状态，不宣称 exactly-once。

错误使用 `AcpError`，`data.kind` 是稳定机器标识：

| code | kind | 含义 |
| --- | --- | --- |
| -32601 | capability_not_negotiated | 当前连接未协商控制能力 |
| -32602 | invalid_request | DTO、名称或配置非法 |
| -32001 | session_not_found / server_not_found | 未建立的 session 或未知 server |
| -32000 | session_closed / pool_closed / server_busy / name_conflict / unsupported | 生命周期拒绝、同名或不可管理目标 |
| -32603 | connection_failed / shutdown_incomplete / internal | 实际执行失败；保留诊断与原因链 |

新 capability 独立允许 `mcp/list`：查询必须携带 `sessionId`，按当前连接门控、精确选池；不得要求外部 API 同时协商 TUI agentEvent 或 OAuth 通知。每条 server 增加 `control: {restart: boolean, stop: boolean, unavailableReason: string|null}`，由服务端依据来源与 system 标记产生，TUI 不从名称或 transport 猜测。原因使用稳定机器值，由 TUI 翻译。现有 TUI/OAuth 查询能力保留，但同样不能为未知 session 回落启动池。

## 4. 单一生命周期

1. 先校验能力、session、DTO、目标与配置，再接纳任务；非法请求不得启动进程或联网。`--bare`、McpMiddleware 关闭及部署 transport 能力缺席均不能经控制面重新开启。
2. 同 pool 同 server 的控制互斥；冲突立即返回 `server_busy`，不得重复启动或交叉关闭。
3. 注册在 pool 内预留名称，写入进程内配置并走统一连接/发现路径。连接失败保留可查询 failed 条目，可 restart/stop；不发布半成品工具。
4. 重启复用原配置；先撤销旧 handle 的新调用权限、停止并 join 关联 OAuth/reconnect/subscription/discovery（含 skill discovery），再排空在途调用和关闭旧 service/process，完成后才创建新连接。
5. 关闭保留配置与 stopped/disconnected 条目，重复关闭成功；后续 restart 可恢复。注册同名条目返回 name_conflict，不能隐式 replace。
6. shutdown 不完整时保持不可调用和可重试关闭状态；返回 shutdown_incomplete，禁止报告 stopped 或开始新实例。pool 的 server slot 在 await 前持有待清理 service/process；超时、waiter 取消或 worker 被 abort 后仍保留相同 owner 与关闭事务，重试不能丢失资源。
7. 旧 bridge、迟到 discovery/OAuth/notification 必须核对当前 handle/连接代次，不能重新发布已关闭实例或覆盖新实例。本版临时注册禁用持久化 resource cache，内存缓存仍须绑定当前连接身份。
8. initialize、OAuth、reconnect 与三个控制动作共用 per-server 准入门和连接代次；建连前取得代次，提交时复核，stop 之后旧初始化不能迟到发布。pool begin-close 后拒绝控制准入/连接发布。任务继续纳入现有 non-Clone McpTaskOwner；关闭顺序遵循 ARC-HOST-SHUTDOWN-001。
9. stdio 复用既有进程隔离、执行 cwd、环境与平台准入；HTTP 复用部署网络能力。客户端配置不能伪造可信来源。

## 5. 能力目录与可观察性

1. pool 是唯一 MCP 连接事实源；工具/资源/skill/command 投影从当前可调用 handle 推导。
2. 下一次 Reason 刷新 MCP 目录，原子发布工具与 ToolSearch 索引；本次 Reason 的 dispatch 固定同一快照。关闭后旧快照的 bridge 也必须拒绝新调用；App/host 工具、resource/skill 激活等入口共用该连接调用 gate，不能绕过撤销。
3. 保留通用 Reason 目录刷新、固定快照与 allowlist/disallowlist。删除仅服务旧 Dynamic MCP 的 overlay 与静态遮蔽/恢复。普通订阅、Git Watch 与任务通知迁移为通用 `McpNotification` 消息源，保持原有 Info/Defer 与持久化历史可读语义。
4. stop/restart 在成功返回前撤销该 server 的资源、skill、command、agent 投影；discovery 任务键绑定 session/registry 身份，避免共享环境的重复键冲突；恢复后重新发现，迟到提交不能复活旧数据。frozen prompt 不重建。
5. 运行状态通过既有 `mcp/list` 查询；TUI 操作结束刷新当前会话快照，失败可见，不依赖通知证明动作完成。

## 6. TUI

1. MCP 详情提供重启/关闭动作，按稳定 server 名执行；不可管理类型隐藏或禁用并给原因。
2. 请求携带当前 session ID，使用既有 OperationScope / CompletionGate；同目标在途操作不重复发送，切换 session 后旧完成不能污染新视图。
3. 成功/失败与重启中/关闭中反馈按既有 i18n、主题和快照提交入口实现。控件不得直调 pool 或 middleware。取消等待、超时或观察者消失表达 OutcomeUnknown，不自动重发；刷新当前作用域后才能展示已确认结论。

## 7. 验收

按风险做少量完整行为验收，不扩大到无关全量测试：

1. DTO roundtrip 与非法输入、当前连接 capability gate、未知/关闭 session，无副作用。
2. 真实本地 MCP wire：register → 发现/调用 → stop → 旧 bridge 拒绝 → restart → 新 handle 可用；重复 stop、同名注册冲突。
3. session 环境隔离、同 server 并发控制、关闭与迟到连接发布竞态、cleanup 不完整不能报告成功。
4. Reason/ToolSearch 与子 Agent 的过滤、固定快照仍成立；skill/command 旧代不可回写。
5. TUI 操作 wire、稳定目标与切换会话后的完成门；不新增视觉快照测试。
6. 旧 DynamicMCP 名称、专属类型/工具/提示词/配置键/实现与测试全部退出当前源码和当前权威文档。历史月志可保留历史表述。
7. 按变更范围执行 cargo check、相关契约测试、fmt、层依赖与文档/文件规模检查；记录准确退出状态及未执行范围。
