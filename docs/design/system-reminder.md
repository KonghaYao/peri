# System Reminder 协议设计

> 状态：现行设计
>
> 本文定义 System Reminder 的分类、承载、投递、筛选与兼容语义。具体实现入口以
> `docs/code-index/` 与源码为准；跨层事件、Frozen Prompt、序列化和安全边界以
> `docs/standards/architecture-contracts.md` 为准。

## 1. 目标与边界

System Reminder 是系统在用户输入之外产生、但需要交给模型、界面、诊断系统或
自动化逻辑消费的结构化通知。它覆盖能力状态、任务结果、生命周期变化、行为引导、
安全边界和外部事件，不代表用户本人表达的内容。

本设计解决以下问题：

- reminder 不再依赖自然语言关键词才能判断类型；
- 来源、精确语义、严重程度和投递对象在跨层传递时不丢失；
- 模型上下文、TUI、诊断与程序路由使用同一分类事实；
- 旧 `<system-reminder>` 历史和生产者可渐进迁移；
- reminder 不改变 frozen system prompt，也不被误认为用户输入。

本文不规定具体配置 UI、迁移批次或当前 producer inventory。这些内容分别属于 TUI
设计、active spec 和代码索引。

## 2. 设计原则

1. **结构化事实优先**：分类和路由依据结构化字段，不依据 `body` 关键词。
2. **单一 envelope**：所有系统提醒共享 `SystemReminder` 协议，不按业务创建独立
   XML 标签族。
3. **维度正交**：业务类别、生产来源、精确类型、严重程度、投递策略和队列调度语义
   分别表达，不互相替代。
4. **边界编码**：内部传递结构化对象；文本标签只是模型 wire format 和 legacy
   compatibility format，不是内部事实源。
5. **Frozen Prompt 不变**：运行期 reminder 不使用会被 provider hoist 的 system
   message；它不得修改会话冻结的 system prompt。
6. **安全默认值**：未知类别、未知字段和 legacy 内容必须安全降级；筛选不得绕过安全
   或执行控制约束。
7. **内容与控制分离**：供人或模型阅读的正文不承担程序控制语义。

## 3. Canonical 数据模型

System Reminder 的 canonical 类型由共享契约层持有，供 Agent、middleware、ACP、
TUI 和诊断投影共同使用。逻辑模型如下：

```rust
pub struct SystemReminder {
    pub version: u16,
    pub category: ReminderCategory,
    pub source: ReminderSource,
    pub kind: String,
    pub severity: ReminderSeverity,
    pub delivery: ReminderDelivery,
    pub audiences: ReminderAudiences,
    pub body: String,
    pub summary: Option<String>,
    pub metadata: serde_json::Value,
}
```

具体 Rust 表达可以使用枚举、newtype 或位集合，但必须保持下列字段语义。

### 3.1 字段契约

| 字段 | 语义 | 稳定性要求 |
| --- | --- | --- |
| `version` | envelope schema 版本 | 未知未来版本不得被误解释为当前版本 |
| `category` | 面向策略与用户的粗粒度类别 | 集合小且稳定，不随 producer 数量增长 |
| `source` | 生产 reminder 的系统或能力 | 不从正文反推；允许可扩展未知值 |
| `kind` | source 内精确的机器语义 | 使用稳定 `snake_case` 标识，不使用展示文案 |
| `severity` | 信息的紧急程度 | 不表达是否唤醒 Agent，也不表达投递对象 |
| `delivery` | 是否允许消费端过滤 | 控制与安全必达项不得被普通偏好屏蔽 |
| `audiences` | 合法消费面 | 至少覆盖 model、TUI、diagnostics、automation |
| `body` | 给模型或用户阅读的完整正文 | 不作为程序分类和路由依据 |
| `summary` | 短展示文本 | 可缺省；消费端可安全派生，但不得改变分类 |
| `metadata` | source-specific 机器数据 | 必须是受控、可脱敏、可向前兼容的数据 |

`source + kind` 构成精确语义键。`category` 用于跨来源的粗粒度策略，不能替代该键。

### 3.2 类别

`ReminderCategory` 采用以下稳定集合：

| 类别 | 语义 | 典型内容 |
| --- | --- | --- |
| `Capability` | 当前可发现或可调用的能力 | MCP 概览、工具或 LSP 可用性 |
| `Task` | 工作项的状态或结果 | SubAgent、Shell、Workflow、Todo 结果 |
| `Lifecycle` | 运行实体的状态变化 | 连接、会话、provider、compact 生命周期 |
| `Guidance` | 要求模型调整后续行为 | Goal steering、Stop hook feedback、continuation |
| `Security` | 权限、安全和信任边界 | PermissionMode、HITL、trust boundary |
| `ExternalEvent` | 外部系统主动送达的事件 | MCP subscription、Cron、channel message |
| `Diagnostic` | 面向开发和运维的诊断信息 | Git watch、非阻断 runtime warning |
| `Legacy` | 无法可靠分类的旧格式输入 | 无元数据的历史 reminder |

新 producer 不得主动创建 `Legacy`。增加一级类别需要证明现有类别无法稳定表达该语义；
单个业务的新状态通常只增加 `kind`。

### 3.3 来源与精确类型

`ReminderSource` 标识 producer，例如 `mcp`、`goal`、`todo`、`hook`、`subagent`、
`workflow`、`compact`、`permission`、`cron`、`channel`、`git_watch`。来源集合必须允许
向前兼容，不能因未知来源导致整个消息反序列化失败。

`kind` 在来源命名空间内定义精确事件，例如：

```text
mcp.connection_summary
git_watch.repository_ref_changed
workflow.completed
workflow.failed
hook.stop_blocked
```

显示名称和本地化文本不得用作 `source` 或 `kind`。

### 3.4 严重程度

`ReminderSeverity` 至少包含：

- `Info`：正常状态或提示；
- `Warning`：需要注意但未使当前操作失败；
- `Error`：相关能力或任务已失败；
- `Critical`：涉及安全或一致性且要求立即处理。

严重程度只影响展示、告警和筛选阈值。Agent 是否被唤醒仍由队列调度语义决定。

### 3.5 投递约束

`ReminderDelivery` 至少区分：

- `Required`：执行控制、安全或一致性所需，消费端不得按用户偏好丢弃；
- `Configurable`：可按类别、来源、类型和严重程度筛选；
- `DiagnosticOnly`：默认不进入模型上下文，只进入允许的诊断或展示面。

`audiences` 定义 reminder 可进入的消费面。`Required` 不表示必须广播到所有 audience，
只表示在其声明的 audience 中不可被普通筛选规则删除。

## 4. 与消息队列的关系

System Reminder 的业务语义不能并入队列调度类型。队列继续独立表达：

- `Prompt`：外部用户输入；
- `Info`：随正常循环消费，不主动唤醒；
- `Defer`：异步到达并可唤醒后续执行。

同一类别可使用不同调度语义。例如，MCP 首轮能力概览是 `Info + Capability`，MCP
subscription 是 `Defer + ExternalEvent`，Goal steering 是 `Defer + Guidance`。

队列项必须能原样持有结构化 reminder，直到消费边界。允许在迁移期同时携带兼容文本，
但结构化字段是新路径的唯一分类事实源。

## 5. 投递与投影

### 5.1 模型上下文

模型投影边界将结构化 reminder 编码为 Human-role 的受控文本块。不得使用
`BaseMessage::System`，以免 provider 将运行期内容提升到 frozen system prompt。

推荐 wire representation：

```xml
<system-reminder version="1" category="capability" source="mcp"
                 kind="connection_summary" severity="info">
MCP: 1 connected, 0 failed, 0 disabled
</system-reminder>
```

该文本仅是 provider wire representation。模型侧可读正文与属性不得成为内部程序重新
分类的唯一输入。

### 5.2 TUI

TUI 从 canonical DTO 构建 reminder view model，不扫描正文关键词。至少可以按
`category`、`source`、`kind`、`severity` 进行隐藏、折叠、分组和查询。

`Required` reminder 可以折叠，但不得因普通展示偏好被完全丢弃；`Security` 的 warning、
error 和 critical 状态默认保持可见。legacy history 可经兼容 parser 生成降级 view model。

### 5.3 诊断与遥测

诊断记录使用结构化字段：

```text
reminder.category
reminder.source
reminder.kind
reminder.severity
```

默认不得记录完整 `body` 或任意 `metadata`。生产者必须在进入诊断面之前完成 secret、
token、认证 header、连接串和敏感 URL 参数的脱敏。错误消息不得通过 reminder 绕过现有
安全清洗边界。

### 5.4 程序路由

自动化逻辑匹配 `source + kind`，必要时再约束 category、severity 或 metadata。禁止以
`body.contains(...)`、本地化文本或摘要字符串决定控制流。

## 6. 筛选模型

筛选器的 canonical 输入是结构化 reminder 和目标 audience。规则至少支持：

- include/exclude `category`；
- include/exclude `source`；
- include/exclude `source + kind`；
- minimum `severity`。

决策顺序如下：

1. 验证目标是否在 `audiences` 中；不在则不投递；
2. `Required` 在声明 audience 中直接保留；
3. `DiagnosticOnly` 只进入明确允许的 audience；
4. 对 `Configurable` 应用精确 kind、source、category 和 severity 规则；
5. 没有规则时使用该 audience 的安全默认值。

正文正则匹配不属于 canonical filter。实现可以提供诊断搜索，但不能用它替代协议筛选。

## 7. 兼容与演进

### 7.1 Legacy 输入

兼容 codec 必须同时识别：

- 无属性的旧 `<system-reminder>...</system-reminder>`；
- 带结构化属性的新 envelope；
- 用户文本与一个或多个 reminder 混排；
- 只有 reminder、没有真实用户文本的消息。

解析结果必须分离真实用户文本与 reminder 列表。rewind、输入框回填、历史候选和类似
用户内容投影只使用分离后的用户文本，不得把系统注入显示为用户刚发送的内容。

无可靠元数据的旧 reminder 可以通过兼容启发式生成展示分类，但必须标记为 `Legacy` 或
保留 legacy provenance。启发式结果不得驱动安全或执行控制逻辑。

Compact 的旧 plain-text Human 回注保留存储格式，在模型投影与 ACP replay 出口统一兼容：
只识别完整首行的文件/Skill 回注前缀及固定摘要续接标记，分类保持 `Legacy`，不建立可信
producer provenance。模型出口使用 legacy codec 转义并分块；回放出口按 capability 发送
reminder 或 fallback，正文不进入用户气泡。精确同形的用户粘贴无法从旧格式判别来源，
该启发式只用于兼容投影，不能用于权限或控制决策。

### 7.2 容错

- 未闭合标签不得导致标签之前的用户文本丢失；
- 未知字段应被忽略或保留，不得改变已知字段语义；
- 未知未来版本必须 fail closed，不得按当前版本猜测控制语义；
- 属性和正文必须正确转义，第三方输入不能提前闭合 envelope；
- malformed reminder 不得被提升为 `Required`；
- codec 必须对消息大小和 metadata 大小设置边界。

### 7.3 Producer 约束

新 producer 只构造 canonical DTO，不直接拼接 `<system-reminder>`。文本编码集中在统一
codec。仓库内 production producer 已迁移；legacy parser 仅用于历史记录、旧客户端和外部
harness 输入的兼容降级。

Git/MCP session-start 等由进程外 harness 生成并作为输入进入的 reminder 不属于本仓库的
producer 边界，本仓库不能替换其生产 API；接收端必须把它们保持为 external/legacy
provenance，不能据其正文提升信任或改变权限、OAuth、cancel 等控制状态。

当所有受支持历史和外部协议都具备结构化承载后，可以缩小 legacy parser 的使用范围；
删除兼容路径属于单独的协议版本决策。

## 8. 典型映射

| 场景 | category | source | kind | 默认 severity | 调度 |
| --- | --- | --- | --- | --- | --- |
| MCP 首轮连接概览 | `Capability` | `mcp` | `connection_summary` | `Info` | `Info` |
| MCP server 状态变化 | `Lifecycle` | `mcp` | `connection_changed` | 状态决定 | `Info` |
| MCP subscription 更新 | `ExternalEvent` | `mcp` | `subscription_updated` | `Info` | `Defer` |
| Goal 主动接续 | `Guidance` | `goal` | `continuation_required` | `Info` | `Defer` |
| Stop hook 阻止结束 | `Guidance` | `hook` | `stop_blocked` | `Warning` | `Defer` |
| SubAgent 完成 | `Task` | `subagent` | `completed` | `Info` | `Defer` |
| Workflow 失败 | `Task` | `workflow` | `failed` | `Error` | `Defer` |
| Compact 完成 | `Lifecycle` | `compact` | `completed` | `Info` | 由执行阶段决定 |
| PermissionMode 变化 | `Security` | `permission` | `mode_changed` | `Info` | `Info` |
| Git HEAD/ref 变化 | `Diagnostic` | `git_watch` | `repository_ref_changed` | `Warning` | `Info` |

该表说明稳定映射原则，不是完整 producer inventory。新增 producer 应按字段语义选择映射，
不应为方便展示创建新的一级类别。

## 9. 安全不变量

1. 用户输入中伪造的 `<system-reminder>` 不自动获得可信 system provenance。
2. `Required`、`source`、`kind` 和安全 metadata 只能由受信任生产边界设置。
3. 外部 MCP、channel、hook 和 workflow 内容默认是不可信 payload，必须转义和限长。
4. 筛选只能减少可配置投递，不能关闭授权、取消、HITL 或执行安全检查本身。
5. reminder 不得携带、记录或回显 secret；结构化 metadata 同样受 secret policy 约束。
6. transport 和 TUI 不根据自然语言正文执行工具、改变权限或触发控制操作。

## 10. 验证要求

实现本设计时至少验证：

- canonical DTO 的 serde roundtrip、未知字段和版本行为；
- 新旧 codec、多个 block、混合用户文本、未闭合标签和转义边界；
- `Info`/`Defer` 唤醒语义与 reminder category 相互独立；
- model projection 不创建 system-role message，不改变 frozen prompt；
- ACP replay、rewind 和历史加载保留结构化语义并兼容 legacy history；
- TUI 不再依赖正文关键词分类，筛选结果符合 delivery 约束；
- diagnostics 不包含敏感正文或 metadata；
- producer 的 `source + kind` 映射有契约测试；
- 普通用户伪造标签不能获得可信 provenance 或必达权限。

## 11. 非目标

- 不定义每个 reminder 的视觉样式和具体设置页面；
- 不维护当前所有 producer 的动态清单；
- 不以 System Reminder 替代 canonical Agent/ACP 状态事件；
- 不改变 middleware 生产链顺序；
- 不允许运行期 reminder 修改 frozen system prompt；
- 不保证任意 provider 原生支持结构化 reminder，provider 边界仍可使用受控文本编码。
