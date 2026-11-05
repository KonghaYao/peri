# MCP 生态定位与互通指南（Connector 视角）

> 本文件是生态背景与外部互通参考，不是仓库架构或协议的事实源。Perihelion
> 的 MCP 设计以 `docs/design/acp-mcp-control.md`、
> `docs/design/mcp-multiplexing.md`、`docs/design/mcp-adaptation-v4-part-1.md`
> （MCP 迁移范围与验收语义的目标设计）和代码契约测试为准。
>
> 实验位置：`side-projects/mcp-apps/`（Node.js + `@modelcontextprotocol/ext-apps` + `@modelcontextprotocol/sdk`）
> 文档定位：说明 MCP 在 perihelion 中的生态角色——**标准的 connector / 通用外部能力对接器**——以及外部开发者如何实现「外部 MCP server ↔ peri 内部」的互通。不搬运规范原文，官方出处见各章末尾链接。

## 目录

- [1. 背景与术语](#1-背景与术语)
- [2. MCP 在 perihelion 中的生态定位](#2-mcp-在-perihelion-中的生态定位)
- [3. tools：可执行动作（核心原语）](#3-tools可执行动作核心原语)
- [4. resources：只读内容通道（核心原语）](#4-resources只读内容通道核心原语)
- [5. subscriptions：服务端通知](#5-subscriptions服务端通知)
- [6. MCP Apps：交互式 UI 扩展（SEP-1865）](#6-mcp-apps交互式-ui-扩展sep-1865)
- [7. skills：技能发现与加载](#7-skills技能发现与加载)
- [8. 外部接入指引](#8-外部接入指引)
- [9. Peri 接入配置与事实源](#9-peri-接入配置与事实源)
- [10. 参考](#10-参考)

## 1. 背景与术语

### 1.1 两个协议版本

全文所有差异讨论都围绕两个 MCP 规范版本展开，请先建立这个对照意识：

| 版本 | 状态 | 关键差异 |
| --- | --- | --- |
| 2025-11-25 | 前一版（旧 server 协商回退时使用） | 订阅机制为 `resources/subscribe` RPC |
| 2026-07-28 | 现行规范（peri 依赖的 rmcp `ProtocolVersion::LATEST` 即此版本） | 引入 `subscriptions/listen` 统一订阅流；MCP Apps 成为正式扩展（SEP-1865） |

外部实现者的唯一要求是**按 client 协商的协议版本分支**。本文给出两版差异对照，避免实现时混用。

### 1.2 术语

- **MCP server**：能力提供方，暴露 tools / resources 等原语。
- **MCP client / host**：Peri 宿主；`peri-middlewares` 实现连接与调用，ACP Host 持有部署生命周期；下游 Web Host 渲染 MCP App。
- **原语（primitive）**：tools（可执行动作）、resources（只读内容）、prompts（用户模板，peri 不采用，见 4.1）。
- **MRTR**：Multi-Round Tool Results，多轮工具结果。
- **SEP**：Spec Extension Proposal，规范扩展提案编号（如 SEP-1865 是 MCP Apps）。
- **WG**：Working Group，工作组。
- **HITL**：Human-in-the-Loop，人机确认。
- **ACP**：peri 内部协议。view 层（peri-tui / web UI）只从 peri-acp 接数据，与 MCP 无直接关系。

## 2. MCP 在 perihelion 中的生态定位

### 2.1 定位：标准的 connector

MCP 在 perihelion 中是**标准的 connector——通用的外部能力对接器**。这句话有三层含义：

- **对外**：任何外部能力提供方（工具、数据源、服务、交互界面）实现一个标准 MCP server 即可接入 peri，无需了解 peri 内部协议（ACP），也无需为 peri 定制。
- **对内**：`peri-middlewares` 实现 MCP client，ACP Host 持有部署生命周期；`peri-agent` 消费工具与资源端口。`peri-acp` 是 ACP 数据出口，`peri-tui` / web UI 消费其投影。
- **能力面**：由若干标准能力组合而成——`tools` / `resources`（核心原语）+ `subscriptions`（服务端通知）+ `MCP Apps`（官方扩展）+ `skills`（工作组推进中）。server 按需声明与组合，client 按能力协商。

### 2.2 互通拓扑

```mermaid
flowchart LR
    T["tools · resources<br/>+ listChanged / subscribe 能力声明"]
    N["subscriptions/listen<br/>服务端通知"]
    S["skills（skill:// 技能资源）"]
    A["MCP App（ui:// 资源）"]
    subgraph AGENT["Peri 宿主 —— peri-middlewares MCP client"]
        MC["MCP Client<br/>tools/list · tools/call · resources/read · subscriptions/listen"]
        RELAY["Apps relay ports<br/>raw metadata/result · capability profile"]
    end
    subgraph ACP["peri-acp —— ACP 服务层（数据出口）"]
        SESS["Session / 事件映射"]
        ST["StdioTransport · MpscTransport"]
    end
    subgraph VIEW["ACP client"]
        DOWNSTREAM["下游 UI / Web Host<br/>通过 stdio ACP 消费 Apps 数据"]
        TUI["peri-tui<br/>不声明、不消费 Apps capability"]
    end

    T <-->|"MCP 原生协议（JSON-RPC）"| MC
    N <--> MC
    S <--> MC
    A <-->|"resources/read"| MC
    MC <--> RELAY
    RELAY <-->|"Apps ports / DTO"| SESS
    SESS <--> ST
    ST <-->|"ACP stdio envelope"| DOWNSTREAM
    ST <-->|"MpscTransport（普通 ACP）"| TUI
```

下游 Web Host 与 MCP Apps FE 的 iframe/`postMessage` 数据流不属于 Peri，实现细节不在本拓扑展开。

### 2.3 对外契约与能力索引

**对外人员的唯一契约**：实现标准 MCP server（stdio 或 Streamable HTTP 传输），按需暴露下列能力；互通由 peri 侧（MCP client + ACP 通道）完成，外部无需感知 peri 内部结构。

| 章节 | 能力 | 两版差异 / 状态 |
| --- | --- | --- |
| [3](#3-tools可执行动作核心原语) | tools（核心原语） | 部分（list_changed 发送条件） |
| [4](#4-resources只读内容通道核心原语) | resources（核心原语） | 部分（订阅机制） |
| [5](#5-subscriptions服务端通知) | subscriptions（服务端通知） | **差异显著（重点）** |
| [6](#6-mcp-apps交互式-ui-扩展sep-1865) | MCP Apps（官方扩展） | 正式（2026-07-28 纳入，SEP-1865） |
| [7](#7-skills技能发现与加载) | skills（技能发现） | 未入规范（工作组推进中） |

> prompts 原语因职责已被宿主命令系统与 skills 覆盖，peri 不采用（见 4.1）。

## 3. tools：可执行动作（核心原语）

### 3.1 协议机制

- **发现**：`tools/list`，支持分页与缓存。
- **调用**：`tools/call`。
- **工具结构**：`name`（唯一）、`description`、`inputSchema`（JSON Schema 2020-12），可选 `outputSchema` / `icons` / `annotations`。

tools 原语在两个协议版本中基本兼容。2026-07-28 的新增与调整有三处：

1. 调用结果携带 `resultType`（`"complete"` / `"input_required"`）——即 MRTR（Multi-Round Tool Results，多轮工具结果）机制。
2. list 结果可带 `ttlMs` + `cacheScope`（CacheableResult），与 `listChanged` 互补。
3. `x-mcp-header` 注解：将工具参数镜像为 HTTP 头，仅供 Streamable HTTP 中介路由使用。

### 3.2 进入 peri 的工具面：direct 与 deferred

对 MCP 工具而言，核心问题不是协议本身，而是**如何进入 peri 的 tool search 体系**。peri 把工具面分成两面（由 `BaseTool::is_direct()` 决定，默认 `false`——安全默认值：新工具默认为 deferred）：

- **direct（核心面）**：直接出现在 LLM API 的 tools 参数中，始终可见——core tools 与两个元工具（`SearchExtraTools` / `ExecuteExtraTool`）属于此面。
- **deferred（延迟面）**：不直接可见。工具进入 `ToolSearchIndex` 索引，agent 先经 `SearchExtraTools` 发现、再经 `ExecuteExtraTool` 代理执行。

**MCP 工具默认落在 deferred 面**，收益有二：

- **不占 LLM tools 参数**：N 个 MCP server 的工具全部注入会挤爆上下文与 API 参数面；延迟加载让工具 schema 只在被搜索命中时才出现。
- **工具面可控**：新接入的 server 不改变 core 工具的可见性，agent 的稳定工具面保持不变——这是「N 个 server 产生 N 个碎片化入口、工具面失控」问题的通用解法（skills 同理，见第 7 章）。

**例外**：声明为启动依赖的 server 用 `system_mcp_tools` 指名必需工具（配置语义见 §9.2）；这些工具的目标行为是不经 tool search、直接进入 RCRA 工具列表，契约见 `docs/design/mcp-adaptation-v4-part-1.md`。其余 MCP 工具仍全部落在 deferred 面。

**内置实例（builtin）**：peri 自带的 Web / Artifact / Cron / Monitor / Workspace / Workflow 能力同样以 MCP 实例形态提供，只是 server 与 client 同进程，由本地默认配置层自动注入。各实例的工具、直连性与关闭键见 [MCP packages 索引](../code-index/mcp-packages.md)；声明表 `peri-acp-types/src/builtin_mcp.rs` 是唯一事实源。

### 3.3 桥接与命名

MCP 工具经 `McpToolBridge`（`peri-middlewares/src/mcp/tool_bridge.rs`）包装为 `BaseTool`：

- **命名**：普通 MCP 工具与 deferred MCP 工具使用 `mcp__<server>__<tool>`，server / tool 名经 sanitize（非 `[a-zA-Z0-9_-]` 字符替换为 `_`）；技能命令使用独立词法（见 §7.4）。被 `system_mcp_tools` 选中的工具以 wire 原始工具名注入模型面。
- **内置实例工具名**：builtin `web`、`artifact`、`workspace` 的 direct 工具以原始名出现在模型面（`WebSearch`、`WebFetch`、`artifact`、`Read`、`Write`、`Edit`、`Glob`、`Grep`、`folder_operations`、`Bash`）；builtin `cron` / `monitor` / `workflow` 保持 deferred，模型面名为 `mcp__<实例>__<tool>`。普通 system MCP 的选中工具同样使用原始名，其余工具仍带前缀。MCP wire `tools/call.name` 始终使用原始工具名，原名注入不创建前缀别名。
- **按绑定来源判定与匹配**：权限按工具来源和原始工具身份处理；未知或外部 MCP 工具不能因名称恰为 `Read`、`Write` 或 `Bash` 获得 builtin 权限。工具名匹配按实际模型可见名进行，不合成额外可调用别名。
- **描述**：加 `[MCP:<server>]` 前缀，指明来源 server。
- **参数**：透传 MCP 工具的 `inputSchema`。
- **调用**：`invoke` 走 MCP client 的 `tools/call`，超时 120s；输出超过 2000 行或 100 KB 时按 `format_output` 截断并落存到 workspace output store，返回可回查引用（`peri-middlewares/src/mcp/client/output_store.rs`，见 4.5）。

### 3.4 发现与执行链路（tool search 体系）

由 `peri-middlewares/src/tool_search/` 实现，`ToolSearchMiddleware` 编排：

1. **入索引**：`before_agent` 把 deferred 工具构建进 `ToolSearchIndex`——TF-IDF 索引，name 权重 3.0 / description 权重 2.5，CJK 逐字分词、ASCII 按空格 / 下划线 / 连字符分词，余弦相似度排序；同时把「延迟工具列表」（名称 + 摘要）注入 system prompt，让 agent 知道存在哪些延迟工具。
2. **发现**：agent 调 `SearchExtraTools`（direct 元工具，namespace `meta`）——查询形式 `select:<tool_name>`（按名精确取）或关键词搜索；返回命中工具及完整 JSON schema。
3. **执行**：agent 调 `ExecuteExtraTool`（direct）传 `{tool_name, params}` 代理执行 → 路由到 `McpToolBridge.invoke` → MCP `tools/call` → server。

```mermaid
flowchart LR
    A["MCP server<br/>tools/list 声明"] -->|"连接后逐个包装"| B["McpToolBridge<br/>mcp__server__tool<br/>[MCP:server] 描述前缀"]
    B -->|"is_direct() = false（默认）"| C["shared_tools<br/>deferred 面"]
    C -->|"before_agent 构建"| D["ToolSearchIndex<br/>TF-IDF 索引"]
    D -->|"延迟工具列表（名称+摘要）"| E["system prompt"]
    F["agent（LLM）"] -->|"SearchExtraTools<br/>select: / 关键词查询"| D
    D -->|"命中工具的完整 schema"| F
    F -->|"ExecuteExtraTool<br/>{tool_name, params}"| G["McpToolBridge.invoke"]
    G -->|"tools/call"| A
```

> 与 skills 的关系：各 server 的 skill 搜索工具同样不进 agent 工具面，peri 用统一的 discover skills 工具聚合（见 7.4），与 tool search 体系同构。

### 3.5 列表变更通知

- 声明 `capabilities.tools.listChanged`，表示支持在工具列表变化时发送 `notifications/tools/list_changed`（SHOULD 语义）。
- **发送条件因版本而异**：
  - 2025-11-25：连接建立后直接推送。
  - 2026-07-28：仅发往已通过 `subscriptions/listen` 订阅了 `toolsListChanged` 的流（机制见第 5 章）。

### 3.6 实现要点

- **描述质量决定可发现性**：deferred 工具的检索完全基于 `name` + `description` 的 TF-IDF 索引——description 写清「用途 + 适用场景」直接决定 agent 能否搜到它。这一点对 MCP server 作者是最重要的提示。
- 工具列表变化时，按 client 协商的协议版本发送 `list_changed`。
- **两类错误分开处理**：工具执行错误用 `isError: true` 的结果回报（模型可据此自纠）；协议错误用 JSON-RPC error（如 `-32602`）。
- 2026-07-28 新增要求：
  - 工具列表的返回顺序保持确定性，利于缓存与 prompt cache 命中。
  - 敏感参数（密码、API key、PII）**禁止** `x-mcp-header` 注解。

**规范出处**：

- 2026-07-28 · https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- 2025-11-25 · https://modelcontextprotocol.io/specification/2025-11-25/server/tools

## 4. resources：只读内容通道（核心原语）

### 4.1 协议机制与三原语分工

- **发现**：`resources/list`（`uri` 唯一标识）。
- **读取**：`resources/read`（文本 / 二进制）。
- **模板**：`resources/templates/list` 暴露参数化资源模板（URI template + completion 补全）。
- **元数据**：`annotations`（`audience` / `priority` / `lastModified`）。
- 一次 `resources/read` 可返回多个内容块（例如目录资源聚合多文件）；`https://` scheme 下 client 可绕过 server 直接拉取。

resources 是 server 暴露给 client 的**内容 / 数据层（只读）**，官方定位是「为语言模型提供上下文的数据」，可类比为 **LLM 的文件系统**。三原语分工：**tools = 可执行动作**（函数）、**resources = 只读内容**（文件）、**prompts = 用户模板**（职责已被宿主命令系统与 skills 覆盖，peri 不采用）。判断标准：凡是「模型生成回答前需要读的东西」放 resources。

两版协议在 resources 本体上兼容，差异集中在订阅与通知（见 4.8 与第 5 章）。

### 4.2 URI 分工

| scheme | 内容 | 典型消费方 |
| --- | --- | --- |
| `file://` | 类文件系统内容（源码 / 配置 / 目录，不必映射真实磁盘） | agent 上下文 / UI 浏览 |
| `git://` | 版本控制内容 | agent 上下文 |
| `https://` | web 资源，client 可绕过 server 直接拉取 | 双方 |
| `skill://` | **Claude Code 生态约定（Skills Over MCP WG 标准化中）**：`SKILL.md` 技能 | **agent（技能加载器）** |
| `ui://` | **MCP Apps 扩展自定义**：HTML 界面（见第 6 章） | **UI 渲染层（iframe）** |
| 自定义 | `log://`、`telemetry://` 等运行数据；协议不穷尽，RFC3986 合规即可 | 实现决定 |

`ui://` 是特例：不在核心协议的标准 scheme 列表中，由 MCP Apps 扩展定义。它借道 resources 通道（host 用 `resources/read` 拉 HTML），但消费方是 UI 渲染层而非模型。这恰好印证 resources 是「通用数据通道」：**scheme 约定内容，host 决定给谁**。

### 4.3 使用场景

1. **agent 上下文注入**：项目文档 / README、配置文件、数据库 schema、接口定义 → agent 生成回答前读取（`file://`、`git://`）。
2. **参数化资源**：模板 `file:///{path}` 按参数展开（`resources/templates/list` + completion 补全）。
3. **目录资源**：一次 `resources/read` 返回多个文件内容（server 聚合）。
4. **运行数据**：`log://`、`telemetry://` 暴露给 agent 调试，或给 UI 做监控面板。
5. **UI 内容**：图片 / 图表等二进制资源（blob）直接渲染到 view。
6. **MCP Apps**：`ui://` HTML 借道 resources 通道，消费方是 iframe 渲染层（第 6 章）。
7. **技能分发**：`skill://` 前缀的 `SKILL.md` 资源，agent 加载为技能实体（第 7 章）。
8. **热更新**：配合 subscriptions——配置变更 → `notifications/resources/updated` → 重读（第 5 章）。

### 4.4 资源的发现机制

**协议事实**：`resources/list` 仅支持 `cursor` 分页（另可带 `ttlMs` / `cacheScope` 缓存），**没有 query / filter / search 参数**。搜索与过滤被设计为 client 的责任（host 侧交互模式），协议不提供 server 侧检索方法。

「查」的四步链路：

1. **list 发现**：`resources/list` 全量列举（cursor 分页遍历，`ttlMs` 缓存）。
2. **过滤选择**：client 基于 `annotations` 元数据过滤——`audience` 按受众、`priority` 按重要性排序、`lastModified` 按新旧；列表变化靠 `list_changed` 失效缓存。
3. **read 读取**：按 URI 精确读取（文本 / 二进制多内容块）；`https://` 可绕过 server 直拉。
4. **变化重读**：订阅资源 → `updated` 通知 → 重读（第 5 章）。

server 侧按需检索的替代方案是**参数化资源模板**：把查询参数编进 URI template（RFC 6570），`resources/read` 即查询结果。生态标准写法如 `search://{query}`（FastMCP 范式）；配合 **completion API** 补全模板参数（如 `file:///{path}` 的路径补全）。这是最接近「搜索」的原语：URI 即查询，read 即答案。

搜索类需求多数仍走 **tools**：`search_files` / `web_search` 等工具承担全文检索。搜索伴随权限与副作用语义时，tools 有完整的 HITL 通道与结果审计，比资源模板更通用。

**两版差异**：缓存语义在 2026-07-28 增强（`ttlMs` TTL + `cacheScope` 私有 / 公开，替代旧版无 TTL 的列表缓存）；分页与 completion 两版一致。

peri 侧现状：资源读取已由 `McpResourceTool`（`mcp_read_resource`，`peri-middlewares/src/mcp/resource_tool.rs`）统一承接——参数 `{server_name, uri}`，description 内缓存各 server 的资源汇总列表，agent 据此发现可用资源。

### 4.5 资源如何下载到本地（不经过 model）

资源内容默认**不经过模型中转**；由 MCP client（peri-agent 侧）执行 `resources/read` 取回后，按内容形态与受众分三路：

- **`https://` 直拉**：协议允许 client 绕过 server 直接下载（协议特性），server 只负责给出 URI；下载由 host 完成，不占模型上下文。
- **二进制 blob 落盘 / 直渲**：`McpResourceTool` 对 blob 内容只返回 `<N bytes of binary data>` 占位——原始字节不进上下文；`user` audience 的图片 / 图表由 view 层直接渲染（4.3 场景 5）。
- **超长文本落盘**：超过 2000 行或 100 KB 的文本输出经 `format_output`（`peri-middlewares/src/mcp/client/output_store.rs`）落存到 workspace output store，模型拿到 `peri-output://` 资源引用与路径，可用 `mcp_read_resource(server_name=workspace, uri=<引用>)` 或 `Read` 读取。落存失败时输出明确标注 `Full output NOT saved`，不回落宿主文件系统。
- **MCP Apps HTML**：`ui://` 由 host 拉取后本地渲染（第 6 章），HTML 本身不进模型。

一句话：**大内容 / 二进制走「落盘 + 路径引用」，小文本走「上下文注入」，渲染类走「view 直拉」**——三条通道都不需要 model 中转。

### 4.6 资源如何以可读形态给 model

- **文本内容块**（TextResourceContents）：直接进模型上下文——这是 resources 的默认形态（`audience` 默认语义即「给 agent 读上下文」）。
- **二进制内容块**（BlobResourceContents）：不给原始字节；给 `<N bytes>` 提示或本地文件路径，模型按需读文件或交给工具处理。
- **audience 路由（perihelion 落点）**：资源用 `annotations.audience` 标注目标受众——`"assistant"` = 给 agent（模型上下文）；`"user"` = 给用户界面展示，可两者皆标，也可不标。client 读到内容后按 audience 路由：`assistant` 进模型上下文（事件链），`user` / `ui://` 透传 view 层。
- **大文件策略**：截断 + 落盘路径引用（见 4.5），模型读文件而非全量进上下文。

### 4.7 读写语义：只读 + 配套写工具

**协议事实**：MCP resources 只有 `list` / `read` / `templates/list`，**没有写方法**。内容的「可变性」只通过 `notifications/resources/updated`（订阅后）表达，client 收到通知后重读拿新值。写操作在协议语义上归 **tools**（state-changing operations）。

**决策**：MCP 边界上 resources 一律保持只读语义；可变实体用「读资源 + 配套写工具 + updated 通知」配对：

1. 资源 `uri` 承担只读快照（`resources/read`）。
2. 配套写工具（如 `update_<name>`）承担全部状态变更。
3. 写工具成功 → server 发 `notifications/resources/updated`（或 `list_changed`）→ 订阅的 client 重读。

配套关系的几条细则：

- **可发现性**：写工具的 `description` 显式声明其对应的资源 URI。读 URI ↔ 写工具的对应关系是**约定而非机制**——server 负责文档化，client 按约定引导 agent。
- **命名区分（peri 内部）**：`peri-resources`（内部数据访问层，**可读写**：config / sessions / workflow）≠ MCP resources（协议，**只读**）。两者是「内部访问通道」与「对外只读视图」的关系，同名不同义；文档与代码注释须明确所指。
- **边界映射**：peri 内部数据若对外暴露，映射为「只读资源 + 写工具」两半；内部写路径需预留事件出口，才能在订阅存在时触发 updated 通知。
- **不引入自定义写方法**：`resources/write` 一类扩展方法破坏互操作（其他 client 不认），不进入对外契约；仅可在内部实现中预留。

### 4.8 能力声明与资源通知

- 声明 `capabilities.resources.listChanged` 表示支持资源列表变化通知（SHOULD 语义）；声明 `resources.subscribe` 表示支持单资源变更通知。**两者独立可选，可皆不声明。**
- **订阅机制两版不同**：
  - **2025-11-25**：client 发 `resources/subscribe`（`params.uri`）订阅单个资源，变更时收 `notifications/resources/updated`（`params.uri`）；`list_changed` 直接推送。Streamable HTTP 另有 GET 端点（`?method=notifications/resources/updated`）承载订阅流。
  - **2026-07-28**：`resources/subscribe` 被 `subscriptions/listen` 的 `resourceSubscriptions` 过滤器取代；`notifications/resources/updated` 追加 `subscriptionId`（详见第 5 章）。
- 错误码调整：2026-07-28 将「资源不存在」从 `-32002` 改为 `-32602`（client 向后兼容接受两者）。

订阅的资源内容变化（2026-07-28 形态）：

```jsonc
{ "jsonrpc": "2.0", "method": "notifications/resources/updated",
  "params": { "_meta": { "io.modelcontextprotocol/subscriptionId": 1 }, "uri": "file:///project/config.json" } }
```

### 4.9 实现要点

- 读取不存在的资源：返回 `-32602`（2026-07-28）/ `-32002`（旧版）；**禁止**对不存在的资源返回空 `contents` 数组。
- 按消费方标注 `audience`（`assistant` / `user`）并合理设置 `priority`，供 host 决定纳入上下文还是展示。
- 校验并净化 `file://` 路径（防目录穿越）；验证 URI、控制访问、正确编码二进制。

**规范出处**：

- 2026-07-28 · https://modelcontextprotocol.io/specification/2026-07-28/server/resources
- 2025-11-25 · https://modelcontextprotocol.io/specification/2025-11-25/server/resources
- completion：https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/completion
- 缓存：https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/caching
- 分页：https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/pagination

## 5. subscriptions：服务端通知

这是「MCP server 向 client **主动推送**信息」的唯一官方通道，也是两个版本差异最显著的部分，是互通实现的重点。

### 5.1 2026-07-28：`subscriptions/listen` 统一长流

`subscriptions/listen` 是一条从 server 到 client 的**长生命周期通知流**：client 发一次请求，流保持打开直到取消，取代旧版 `resources/subscribe` RPC 与 HTTP GET 端点。完整时序：

```mermaid
sequenceDiagram
    participant C as MCP Client（peri-agent）
    participant S as MCP Server

    C->>S: subscriptions/listen (id=1)<br/>notifications 过滤器：{toolsListChanged, resourceSubscriptions:[uri]}
    Note over S: 回显同意支持的子集<br/>不支持的类别省略
    S-->>C: notifications/subscriptions/acknowledged<br/>_meta.subscriptionId = 1
    Note over C,S: 确认之前禁止发送任何通知

    S-->>C: notifications/tools/list_changed<br/>_meta.subscriptionId = 1
    S-->>C: notifications/resources/updated<br/>_meta.subscriptionId = 1, uri
    S-->>C: notifications/resources/list_changed<br/>_meta.subscriptionId = 1

    alt client 取消
        C->>S: notifications/cancelled（stdio）或关闭流（HTTP）
    else server 主动关闭（shutdown）
        S-->>C: 空结果 resultType:"complete"（优雅结束）
    end
```

订阅生命周期：

```mermaid
stateDiagram-v2
    [*] --> Listening : client 发 subscriptions/listen<br/>notifications 过滤器声明想收的类型
    Listening --> Active : server 发 acknowledged<br/>（回显同意支持的子集）
    Active --> Active : 推送已订阅类型的通知<br/>每条带 _meta.subscriptionId
    Active --> [*] : client 发 notifications/cancelled / 关闭流
    Active --> [*] : server 主动关闭 → 空结果 resultType:complete
    Listening --> [*] : 重连后订阅丢失<br/>（server 不保存跨连接状态）
```

三条核心规则（对应上图）：

1. **显式 opt-in**：server 不得发送 client 未在过滤器中声明的通知类型。
2. **先确认、后推送**：`acknowledged` 必须先于任何通知。
3. **订阅 ID 多路复用**：每条通知的 `_meta.subscriptionId` = 对应 `subscriptions/listen` 请求的 JSON-RPC id；stdio 单通道上靠它关联通知与订阅。断线重连后 client 必须重发 `listen`（server 无跨连接订阅状态）。

### 5.2 2025-11-25：`resources/subscribe` 单点订阅

旧版没有统一订阅通道，通知分两路：

```mermaid
flowchart LR
    C["MCP Client"] <-->|"连接建立后<br/>list_changed 系列直接推送<br/>（无订阅前提）"| S["MCP Server"]
    C -->|"resources/subscribe {uri}"| S
    S -->|"notifications/resources/updated {uri}"| C
    C -->|"resources/unsubscribe"| S
```

- **list_changed 系列**直接向连接发送（工具 / 资源列表变化，无订阅前提，原则见第 3、4 章）。
- **单资源订阅**通过 `resources/subscribe` RPC 建立，变更时收 `notifications/resources/updated`；Streamable HTTP 传输下另有 GET 端点（`?method=notifications/resources/updated`）承载订阅流。

### 5.3 两版差异对照

| 维度 | 2025-11-25 | 2026-07-28 |
| --- | --- | --- |
| 订阅机制 | `resources/subscribe`/`unsubscribe` RPC + HTTP GET 端点 | `subscriptions/listen` 单一长流（取代上述二者） |
| 通知范围 | 连接建立后 list_changed 直接推；资源订阅按 URI 单个订阅 | 全部通知需显式 opt-in 过滤器，**未请求的类别不得发送** |
| 关联性 | 通知不带订阅标识 | 每条通知 `_meta` 带 `subscriptionId`（= listen 请求 id），stdio 上据此多路复用 |
| 确认机制 | 无 | `notifications/subscriptions/acknowledged` 先确认（回显同意子集）后推送 |
| 资源通知 | `notifications/resources/updated`（`params.uri`） | 同名同构，追加 `subscriptionId` |
| list_changed | 直接发送 | 仅发往订阅了对应类型的流 |
| 取消 | `resources/unsubscribe` | `notifications/cancelled`（stdio）/ 关流（HTTP） |
| 重连 | 订阅随连接丢失，需重建 | 同左，且 client **必须**重发 `subscriptions/listen`（server 无跨连接订阅状态） |

### 5.4 server → client 通知全清单（两版汇总）

| 通知方法 | capability 前提 | 2025-11-25 | 2026-07-28 | 说明 |
| --- | --- | --- | --- | --- |
| `notifications/tools/list_changed` | `tools.listChanged` | 直接发送 | 订阅 `toolsListChanged` 后发送 | 工具列表变化 |
| `notifications/resources/list_changed` | `resources.listChanged` | 直接发送 | 订阅 `resourcesListChanged` 后发送 | 资源列表变化 |
| `notifications/prompts/list_changed` | `prompts.listChanged` | 直接发送 | 订阅 `promptsListChanged` 后发送 | prompt 列表变化 |
| `notifications/resources/updated` | `resources.subscribe` | `resources/subscribe` 后发送 | `resourceSubscriptions` 订阅后发送 | 订阅的某 URI 内容变化 |
| `notifications/subscriptions/acknowledged` | — | 无 | 新增（订阅确认） | 先于一切通知 |
| `notifications/progress` | — | 有 | 保留（request-scoped，走请求自身响应流） | 请求进度 |
| `notifications/message` | `logging` | 有 | **废弃**（SEP-2577） | 日志；2026-07-28 起仅当请求 `_meta` 带 `io.modelcontextprotocol/logLevel` 才可发 |
| `notifications/initialized` | — | 有 | **移除**（2026-07-28 取消 initialize 握手） | 初始化完成 |
| `notifications/roots/list_changed` | `roots` | 有 | **移除**（Roots 整体废弃，SEP-2577） | 根目录列表变化 |
| `notifications/elicitation/complete` | — | 有 | **移除**（elicitation 引导式提问，由 MRTR 取代） | 请求 / 响应之外的带外交互完成 |

> 表中 SEP = Spec Extension Proposal（规范扩展提案）编号；WG = Working Group（工作组）。

### 5.5 实现要点与协议版本注意

- **必须按 client 协商的协议版本分支**：`subscriptions/listen`（2026-07-28）与 `resources/subscribe` + 直接 list_changed（2025-11-25）互不通用。rmcp `ProtocolVersion::LATEST` 为 2026-07-28，连接旧 server 时协商回退，实现时以协商结果为准。
- 仅当确实支持相应能力，才声明 `listChanged` / `subscribe` capability。
- 实验验证（`side-projects/mcp-apps`）：`sessionIdGenerator: undefined` + `enableJsonResponse: true` 时，全链路无 session 亦可工作，与 2026-07-28 的无状态方向一致——订阅实现无需依赖连接级会话。

**规范出处**：

- 2026-07-28 · https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/subscriptions（机制）、https://modelcontextprotocol.io/specification/2026-07-28/changelog（变更）
- 2025-11-25 · https://modelcontextprotocol.io/specification/2025-11-25/server/resources（订阅）

## 6. MCP Apps：交互式 UI 扩展（SEP-1865）

MCP Apps 是 MCP `2026-07-28` 规范中 **Extensions 框架的官方扩展**（SEP-1865，由早期 MCP-UI 演进而来），属于「标准之一」：服务器可发布**交互式 HTML 界面**（App），客户端（host）在**沙箱 iframe** 中渲染，App 与 host 之间通过 **postMessage + JSON-RPC** 双向通信。

### 6.1 核心机制

1. **资源形态**：server 以 `ui://` scheme 的 resource 发布 HTML（MIME `text/html;profile=mcp-app`）。
2. **工具绑定**：`tools/list` 返回的工具携带 `_meta.ui.resourceUri`；同时处理 `_meta.ui.visibility`，默认可同时面向 model 与 app。
3. **能力方向**：当前进程在 MCP pool prewarm 前读取一次 `PERI_MCP_APPS`；变量只看是否存在，值不解析（包括空串和 `0`）。存在时，所有初始连接和重连都在 MCP `initialize` 中传播 `io.modelcontextprotocol/ui`；不存在时完全关闭。该 deployment profile 与 Apps `appCapabilities`/`hostCapabilities` 是不同层次。
4. **渲染与通信**：下游 Web Host 拉取 HTML 并自行决定如何渲染与通信；Peri 不实现 iframe、sandbox、CSP、Permissions Policy 或 `postMessage`。
5. **工具结果**：Host/model 发起的调用由下游按 Apps 规范投影为 `ui/notifications/tool-input*` 与 `ui/notifications/tool-result`；App 发起的 `tools/call` 使用匹配原始 id 的标准 JSON-RPC response。Peri 保留 `content`、`structuredContent`、`_meta`、`isError`。
6. **安全模型**：下游负责 iframe/CSP/权限执行；Peri 负责 server/session/tool 归属、visibility、connection capability 和既有权限/HITL 边界。

### 6.2 架构：下游 Web Host · MCP server · Peri deployment capability 的关系

Web Host/iframe 仅为下游消费方，本仓库不实现。MCP Apps/UI capability 来自进程启动环境中的 `PERI_MCP_APPS`，不是 ACP capability：

```mermaid
flowchart LR
    ENV["PERI_MCP_APPS\n存在即启用"]
    P["Peri\nACP stdio + MCP client"]
    S["MCP server\nui:// resource · tools"]
    H["下游 Web Host\n消费 Peri ACP 数据"]
    ENV -->|"进程启动时冻结 deployment profile"| P
    P -->|"初始连接与重连\n传播 UI capability"| S
    P <-->|"ACP envelope\nresource/result/app data"| H
    P <-->|"MCP JSON-RPC\ntools/list · resources/read · tools/call"| S
```

Peri 不创建 iframe、不实现 sandbox/CSP/Permissions Policy、不处理浏览器 `postMessage`，也不实现 MCP Apps FE。下游 Web Host 负责把 Peri 的 ACP 数据接入自己的 Apps host；`peri-tui` 不参与。

Peri 侧接入以 [MCP Apps relay 设计](../design/mcp-multiplexing.md) 与
[ACP wire 契约](../design/peri-acp-protocol.md)为准：

1. `PERI_MCP_APPS` 在进程启动时冻结 deployment profile；初始连接与重连传播相同的 UI capability。
2. 下游 Host 使用 `peri/mcp/apps/list|attach|request|cancel|detach|close|rerun`。
   `attach` 消费初始 canonical invocation 签发的 `McpAppRecord` 并绑定当前 ACP connection；
   工具调用共用既有 Permission/HITL 执行路径。
3. `request` 承载允许的 App 操作，保留 MCP resource/result 标准字段与 `_meta`；
   请求 namespace、序号、去重与授权由 facade 校验，不能把任意 JSON-RPC 直接发给 MCP peer。
4. iframe、sandbox、CSP、Permissions Policy 与浏览器 `postMessage` 由下游 Web Host 实现。

### 6.3 ACP 接入边界

`peri/mcp/{open,resource,app,invoke}` 仅是既有客户端的兼容别名；新接入使用
`peri/mcp/apps/*`，不使用旧 lease 或 `invocationToken` 模型设计新客户端。
Apps payload 与 ACP 外层请求身份分别处理；完整 DTO、方法对应及生命周期只在权威设计维护。

### 6.4 关键 JSON-RPC 方法（当前 SDK/实验观察；不是封闭方法全集）

MCP Apps 协议会随 capability 扩展增加 `ui/*`、context、link、message、teardown 等交互；下表只列当前研究涉及的代表性消息，不应作为实现层的永久白名单。

| 方向 | 方法 | 说明 |
| --- | --- | --- |
| App → Host | `ui/initialize` | 握手；**params.appInfo 必含 name + version**（zod 校验，缺 version 报 `-32603`） |
| App → Host | `ui/notifications/initialized` | 初始化完成通知 |
| App → Host | `tools/call` | App 主动回调 server 工具（参数 `{name, arguments}`） |
| App → Host | `ui/notifications/size-changed` | 自适应尺寸通知 |
| Host → App | `ui/notifications/tool-input-partial` | 初始工具调用的增量输入（可选） |
| Host → App | `ui/notifications/tool-input` | 初始工具调用的完整输入 |
| Host → App | `ui/notifications/tool-result` | 初始工具调用结果；完整保留 `content`、`structuredContent`、`_meta`、`isError` |
| Host → App | `ui/notifications/tool-cancelled` | 初始工具调用被取消 |
| Host → App | `ui/resource-teardown` | Host → App request；需要按原 request id 返回 response |
| Host → App | `ui/notifications/host-context-changed` | 宿主上下文变化 |

```mermaid
sequenceDiagram
    participant A as MCP App FE（下游）
    participant H as Web Host（下游）
    A->>H: ui/initialize request
    H-->>A: ui/initialize response（hostCapabilities/hostContext）
    A->>H: ui/notifications/initialized
    H-->>A: ui/notifications/tool-input-partial（可选）
    H-->>A: ui/notifications/tool-input
    H-->>A: ui/notifications/tool-result 或 tool-cancelled
    A->>H: tools/call request
    H-->>A: JSON-RPC response（同一 App request id）
    H->>A: ui/resource-teardown request
    A-->>H: JSON-RPC response
```

Peri 不实现上述 Web Host ↔ App 的 handshake；Peri 只承载下游选择透传的 payload，并负责自己的 ACP connection capability、MCP server capability propagation、server/session/tool 路由和结果完整性。
### 6.5 生态与 SDK 现状

- **官方 SDK**：`@modelcontextprotocol/ext-apps`（前端 `App` 类 + server 端 `registerAppTool` / `registerAppResource` helpers），TypeScript。
- **已支持客户端**：ChatGPT、Claude（web / Desktop，本地需隧道）、VS Code（1.100+）、Goose（experimental / draft 实现）、Postman、MCPJam、mcp-use、官方参考实现 `basic-host`。
- **协议版本注意**：peri 依赖的 rmcp `ProtocolVersion::LATEST` 为 `2026-07-28`；ext-apps 的代码路径已是新协议风格（无 session 模式，见 5.5）。

### 6.6 实验踩坑（`side-projects/mcp-apps`）

1. **`appInfo.version` 必填**：`new App({ name })` 缺 version，host 侧 zod 校验失败返回 `-32603 InternalError`（错误消息含 `path: ["params","appInfo","version"]`）。构造时必须传 `{ name, version }`。
2. **host 资源缓存**：Goose 把拉到的 HTML 缓存到 `~/.config/goose/mcp-apps-cache/`，改 UI 后必须清缓存（或等缓存过期）才能看到新版。
3. **Streamable HTTP 头**：`Accept: application/json, text/event-stream` 必须同时声明；`MCP-Protocol-Version` 按协商版本发送。
4. **Goose 的 MCP Apps 是 experimental**（draft spec 起家），但已 bundle ext-apps 的 stable schema，双向兼容稳定版 App SDK。

**规范出处**：

- 官方扩展仓库：https://github.com/modelcontextprotocol/ext-apps
- 构建教程：https://modelcontextprotocol.io/extensions/apps/build

## 7. skills：技能发现与加载

### 7.1 定位

**技能（skill）= 给 agent 的结构化指令包**：`SKILL.md`（frontmatter 元数据 + 正文指令），教 agent「在特定任务下如何做」，区别于 tools（函数）与 prompts（用户模板）。

技能目录与正文分离：支持 Skills extension 的 server 通过 `skills/list` 发布目录，正文仍经资源读取；旧 server 可仅以 `skill://` 具体资源暴露技能。Peri 的发现与校验边界见 §7.4。

### 7.2 生态参考与工作组

- **Peri 接入**：提供只读查询、异步发现与统一激活；当前目录协议、正文校验及命令形态见 §7.4。
- **安全边界**：技能正文是来源内容，不能凭声明取得工具执行权限；Peri 仍按工具准入与审批契约执行。
- **官方正在标准化**：MCP 官方 **Skills Over MCP 工作组**（2026 年 4 月成立）当前方向为 **SEP-2640 Skills Extension**（Extensions Track，**基于 Resources 原语**），并协调 Agent Skills 规范（agentskills.io 的 well-known URI 发现）与 registry `skills.json`。**尚未进入 2026-07-28 规范正式扩展**（当前正式扩展：Apps / Tasks / Authorization）。

### 7.3 使用场景

1. **server 分发领域技能**：如「如何正确调用本 server 的 API」「本服务的配置惯例」——agent 遇到相关任务时自动加载。
2. **组织级技能库**：registry `skills.json` 分发，多 server 共享技能集。
3. **渐进式披露**：技能先于工具暴露使用说明（与 Primitive Grouping WG 思路一致），减少无谓的工具探索。

### 7.4 Peri 的发现与激活边界

- **本地来源**：builtin `workspace` 实例提供项目、用户、插件与内置技能；宿主聚合工具只消费 MCP 来源，不回落磁盘。
- **发现优先使用 `skills/list`**：server 声明 `io.modelcontextprotocol/skills` extension 后，
  宿主按分页技能目录发现并校验 frontmatter；有完整资源清单时，在激活时核对 digest。
  仅在未声明该能力时，兼容扫描 `resources/list` 中的 `skill://` 资源。
  已声明能力但请求失败时明确报发现失败，不伪装为空目录或退回旧扫描。
- **正文统一激活**：绑定资源经 `resources/read` 校验；stale 快照可经 `skills/get` 刷新。
  技能模板不是具体条目，不能仅靠 `resources/templates/list` 展开出完整技能目录。
- **命令与工具分别命名**：受信系统来源投影为 `core:{skill}`（显示 `/skill`），普通外部来源投影为
  `{server}:{skill}`（显示 `/server:skill`）。`system_mcp` 启动依赖标记不赋予系统来源身份；
  `mcp__<server>__<tool>` 是工具名规则，不能用作技能命令。
- **内容不授予权限**：frontmatter 和正文仍是来源内容，不改变工具准入与审批。
  连接、发现、断连清理与命令投影的具体入口见
  [Middleware 索引](../code-index/peri-middlewares.md)；命令契约见
  [Command 系统](../design/command-system.md)。

**规范出处**：

- Skills Over MCP WG charter：https://modelcontextprotocol.io/community/working-groups/skills-over-mcp
- SEP-2640 Skills Extension：https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2640
- Agent Skills spec：https://agentskills.io/
- 实验参考实现：https://github.com/modelcontextprotocol/experimental-ext-skills

## 8. 外部接入指引

本章汇总全文结论：**外部人员如何实现「外部 MCP server ↔ peri 内部」的互通**。原则一句话：外部人员只实现标准 MCP server，不接触 peri 内部协议。

### 8.1 互通边界与职责划分

- **互通入口**：宿主侧 `peri-middlewares` MCP client（stdio 或 Streamable HTTP），由 ACP Host 管理部署生命周期；`peri-agent` 消费工具与资源端口。`peri-acp` 向 UI 投影结果，`peri-tui` / web UI 不持有 MCP 连接。
- **外部职责**：实现标准 MCP server（可复用官方 SDK：`@modelcontextprotocol/sdk` + 可选 `@modelcontextprotocol/ext-apps`），按第 3–7 章声明能力、暴露工具 / 资源 / 通知 / App / 技能。
- **内部职责**：`peri-middlewares` 负责连接、协商、工具桥接与订阅；宿主按 Permission/HITL 边界调用工具，ACP 负责事件与 Apps facade 投影。
- **peri 侧配置**：server 在 `mcpServers` 中的配置入口与启动依赖见 §9.2；协议由实际连接协商，不配置固定 `protocolVersion`；外部实现者只需满足标准 MCP，不需要了解这些 key。

### 8.2 外部实现 checklist

1. **基础原语**（第 3、4 章）：实现 `tools/list` + `tools/call`（或仅实现 `resources/*` 亦可），声明 `capabilities`。**可变实体遵守读写配对约定**（见 4.7）：资源只读，配套写工具在 description 中声明对应资源 URI，写成功后发 `updated` 通知。
2. **服务端通知（可选，第 5 章）**：声明 `listChanged` / `subscribe` capability；按协商的协议版本实现 `subscriptions/listen`（2026-07-28）或 `resources/subscribe` + 直接 list_changed（2025-11-25）。
3. **MCP App（可选，第 6 章）**：以 `ui://` resource 发布 HTML，工具携带 `_meta.ui.resourceUri` 绑定；App 内用 ext-apps `App` 类（`{ name, version }` 必填）。
4. **技能（可选，第 7 章）**：声明 Skills extension，提供 `skills/list` 与可读取的技能正文；完整资源清单的 digest/frontmatter 须一致。旧 server 可通过 `resources/list` 暴露 `skill://` 具体资源走兼容路径；普通外部技能命令显示为 `/server:skill`。发现与激活约束见 §7.4。
5. **安全基线**：HITL 确认、工具注解视为不可信、敏感操作提示确认；App 场景遵守 iframe 沙箱 + CSP 默认同源 + 权限默认全禁。

## 9. Peri 接入配置与事实源

### 9.1 当前能力与边界

本指南不维护第二份内部实现清单或支持度矩阵。接入前按所需能力核对当前事实源：

| 接入内容 | 权威设计与实现入口 |
| --- | --- |
| 工具发现、调用与模型可见名 | [工具系统](../design/tool-system.md)、[Middleware 索引](../code-index/peri-middlewares.md) |
| System MCP 启动依赖与 builtin 实例 | [MCP 适配设计](../design/mcp-adaptation-v4-part-1.md)、[MCP packages 索引](../code-index/mcp-packages.md) |
| 会话 MCP 注册、重启与关闭 | [ACP MCP 运行控制](../design/acp-mcp-control.md) |
| 标准 MCP Tasks、任务归属与取消 | [Session 异步任务入口](../design/session-async-tasks.md)、[Middleware 索引](../code-index/peri-middlewares.md) |
| 资源订阅与提醒送达 | [System Reminder](../design/system-reminder.md)、[Git Watch](../design/git-watch-middleware.md) |
| Apps relay 与下游 Web Host 边界 | [MCP Apps relay](../design/mcp-multiplexing.md)、[ACP wire](../design/peri-acp-protocol.md) |
| 缓存策略与关闭 | [MCP 响应缓存](../design/mcp-cache.md) |

### 9.2 配置入口与启动依赖

外部 server 通过项目 `.mcp.json` 或用户 `~/.peri/settings.json` 的 `mcpServers` 接入，
使用 stdio `command` 或 HTTP `url`。协议版本由连接协商；具体字段、别名与校验的
事实源为 `peri-acp-types/src/plugin.rs::McpServerConfig`，来源合并见
[配置权威面](../design/configuration-authority.md)。

| 字段 | 使用含义 |
| --- | --- |
| `system_mcp` | `true` 时必须在 RCRA 启动前 ready；未声明或 `false` 按普通 MCP 生命周期连接 |
| `system_mcp_tools` | 同 server 的必需工具原名；仅能与 `system_mcp: true` 配合，空数组只要求 ready |
| `system_mcp_timeout` | 启动等待预算（毫秒）；默认值与合法范围由 `McpServerConfig` 常量和校验定义 |

无效配置明确失败，不能降级为空配置；启动与工具注入契约由
[MCP 适配设计](../design/mcp-adaptation-v4-part-1.md)维护。

builtin 实例由默认配置层提供。用户关闭某个实例时使用
`{"mcpServers":{"web":{"disabled":true}}}` 这一配置形态；不要同时声明
`system_mcp: true`，也不要用 `command` / `url` 接管 builtin 保留名。
实例保留名、工具与策略关闭键以声明表和 [MCP packages 索引](../code-index/mcp-packages.md)为准；
进程级 `PERI_MCP_BUILTIN` 的合法值与关闭边界见
[环境变量标准](../standards/environment-variables.md)。

## 10. 参考

- MCP 规范（2026-07-28 现行）：https://modelcontextprotocol.io/specification/2026-07-28/
- MCP 规范（2025-11-25，前一版）：https://modelcontextprotocol.io/specification/2025-11-25/
- 官方扩展总览：https://modelcontextprotocol.io/extensions/overview
- Skills Over MCP WG：https://modelcontextprotocol.io/community/working-groups/skills-over-mcp
- 官方扩展仓库：https://github.com/modelcontextprotocol/ext-apps
- 构建教程：https://modelcontextprotocol.io/extensions/apps/build
- Goose 教程：https://goose-docs.ai/docs/tutorials/building-mcp-apps（draft-spec 手写 client 写法，可见其演进前形态）
- 2026-07-28 发布说明：https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate（Extensions 框架、Tasks、MCP Apps、授权总览）
