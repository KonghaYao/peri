# v4 云架构与生态审计

状态：部分架构方向已确认，尚未形成实施契约。

用户裁决：同意持久状态、Agent 计算与工具环境分离，同意计算实例可替换、执行可恢复，同意 ACP 协议与可自定义传输层分离，同意 Orchestration 的独立职责与生命周期；已更新根 `CLAUDE.md` 的项目目标。Host/Deployment 概念仍需解释，尚未纳入目标；生态基础设施扩展暂缓。以下保留审计建议，未确认部分不构成已批准设计。

核查日期：2026-09-27。本文区分官方资料事实与对 Peri 的建议；资料反映核查时状态，未实施云部署或故障实验。

## 审计前提与总体判断

用户已确定：本地与云端共用核心，分阶段落地；本轮只讨论架构，不设计接口或实施细节。各项采纳状态以上述裁决为准。

现有 Harness、Sessions、Resources、Orchestration、Endpoint 覆盖了主要功能职责；v4 还需要明确状态、执行环境和宿主的关系。建议保留五个概念，补充横跨它们的 Host/Deployment 架构维度，不据此强制拆成微服务。

建议的大目标表述：Peri v4 建立本地与云端共用的 Agent 执行核心，将持久状态、Agent 计算与工具执行环境解耦；内部能力以 MCP 接入，对外业务交互以 ACP 统一，由可替换宿主提供部署、调度、身份与资源治理。核心不绑定具体进程生命周期、存储后端或云厂商，先完成本地形态，再逐步验证远程能力与可恢复的云端执行。

## 五个概念的调整建议

| 概念 | 应明确的架构职责 |
| --- | --- |
| Harness | 定义 Agent 执行语义与生命周期，不直接拥有某台机器的执行环境；模型推理服务、Harness 计算、工具执行可以位于不同位置 |
| Sessions | 会话历史与执行恢复状态的持久化边界；会话、一次任务运行、承载它的计算实例具有不同生命周期 |
| Resources | MCP 能力与工具环境边界；工作区、进程和产物可以独立于 Harness 实例存在，访问能力不自动授予资源所有权 |
| Orchestration | 拥有任务关系、协调与等待的领域语义；Middleware 提供接入，编排生命周期不绑定某个活跃 Harness 实例，状态经持久化边界保存 |
| Endpoint | ACP 是统一业务交互协议，stdio 是本地传输方式；连接寿命与任务寿命由部署形态明确约定 |

Host/Deployment 负责将这些职责装配为本地进程、自托管服务或云宿主，并提供身份、执行准入、调度与资源治理。它是部署与控制职责，不是新增一套 Agent 业务实现。现有 Controller/Runtime 可作为演进落点，暂不要求新增 crate。

## 仓库证据与架构差距

- [远程存储装配](../../peri-resources/src/sessions/remote/composition.rs) 已明确区分远端数据与本机执行事实。这是存算分离的基础，但不等于支持任意云 worker 接管。
- [后台任务模块](../../peri-agent/src/agent/async_tasks.rs) 明确 Task 为易失投影、重启不复活；云端持久执行需要另行确定 Run 的恢复契约，不能从消息保存推导执行恢复。
- [Agent 依赖](../../peri-agent/Cargo.toml) 仍包含存储、资源和 OS 相关依赖。应将可移植执行核心与本地宿主装配区分，但本轮不决定具体拆分文件。
- [MCP 目标设计](../../docs/design/mcp-adaptation-v4-part-1.md) 同时讨论逻辑能力和实例/进程隔离。建议进一步区分协议边界、信任边界和物理部署边界：能力保持隔离，物理部署由权限、故障范围和生命周期决定；不能从“采用 MCP”推导必须一能力一进程。
- [总体架构](../../docs/design/architecture.md) 与 [设计索引](../../docs/design/README.md) 仍有旧的事实优先级措辞；若采纳本审计，统一按 [标准索引](../../docs/standards/index.md) 区分现状与目标，再同步受影响设计。本轮不自动批准或改写它们。

## 协议与生态边界

MCP `2026-07-28` 明确请求自包含，应用状态跨请求存在时通过显式身份引用；协议连接不等于业务会话。仓库 [MCP transport](../../peri-middlewares/src/mcp/client/transport.rs) 已优先该版本。建议保持内部能力 MCP 化，同时分清系统必需能力与模型可选择工具：持久化、授权和恢复由系统确定性执行，不交由模型决定。参见 [MCP 基础规范](https://modelcontextprotocol.io/specification/2026-07-28/basic)。

ACP 官方将协议与传输分开，当前 transport 页面仍把 Streamable HTTP 标为草案，同时允许保留协议语义的自定义传输。建议目标表述采用“ACP 统一业务出口，stdio 为本地 transport”，云端传输单独适配。参见 [ACP transports](https://agentclientprotocol.com/protocol/v1/transports)。

云基础设施在边界适配：身份系统、存储、观测和运行平台沿用各自生态，MCP 服务内部可使用对应原生能力；业务核心不绑定某云 SDK。ACP 统一面向客户端的业务语义，无需把基础设施健康检查、遥测等也重新发明成 ACP 业务接口。

外部 Agent 联邦可以在出现真实跨产品协作需求时，通过适配器评估 A2A；它的任务与发现语义不应强行套到内部同构 Subagent 上。参见 [A2A 规范](https://a2a-protocol.org/latest/specification/)。本轮不建议加入 v4 必交付范围。

## 云生态观察与架构建议

### 1. 以可恢复执行定义 serverless 目标

**事实**：AWS Lambda durable functions 通过 checkpoint/replay 恢复执行，并提供可释放计算资源的等待机制；Cloudflare Workflows 也以可独立重试的步骤组织执行，休眠可能丢失内存状态。参见 [AWS durable functions](https://docs.aws.amazon.com/lambda/latest/dg/durable-functions.html)、[Cloudflare 工作流规则](https://developers.cloudflare.com/workflows/build/rules-of-workflows/)。

**建议**：v4 应分别定义计算部署位置、执行状态持久性、工作环境生命周期。持久化消息只是其中一部分；运行进度和等待状态也须明确归属。先明确需要“重启后重新发起任务”还是“从已确认边界续跑”，再选择运行时。serverless 是可选部署形态，不宜变成必须采用短生命周期函数的限制。

### 2. 给副作用及不确定结果制定契约

**事实**：AWS 明确 durable step 默认具有至少一次执行语义。已完成 checkpoint 可复用，但步骤完成前中断可能重复执行；启动去重与步骤副作用去重是两个问题。Cloudflare 也要求考虑步骤重试与幂等。参见 [AWS 幂等性说明](https://docs.aws.amazon.com/lambda/latest/dg/durable-execution-idempotency.html)、[Cloudflare 工作流规则](https://developers.cloudflare.com/workflows/build/rules-of-workflows/)。

**建议**：执行恢复与外部副作用之间需要明确责任边界，恢复不能无条件重复工具动作。不能从 MCP、队列或 durable runtime 推导出所有工具均 exactly-once。

### 3. 将可信控制与代码执行环境分开

**事实**：AgentCore Runtime 的 microVM 提供会话级 CPU、内存、文件系统隔离；实例状态与长期持久化状态不同。其安全文档还指出，VM 内代码能够接触执行角色凭据，因此角色权限必须受限。参见 [AgentCore runtime 生命周期](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-how-it-works.html)、[安全最佳实践](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-security-best-practices.html)。

**建议**：控制面负责身份、权限策略、调度与运行状态；Agent worker 承载推理；工具环境承载 shell、构建、浏览器及工作区。三者是责任边界，可以先在单进程部署，但不要共享无限权限或把沙箱存活当作 Run 存活。计算核心可替换，并不意味着编译环境、浏览器、LSP 或工作区必须随每次推理重建。工作区成果、补丁和大体积产物应有独立身份与持久化生命周期。

### 4. 多租户隔离包含授权与资源治理

**事实**：AgentCore 明确不负责 session-to-user 映射，应用后端必须维护用户与会话关系，并管理每用户会话数量。基础设施隔离不能替代应用授权。参见 [AgentCore 安全最佳实践](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-security-best-practices.html)。

**建议**：云形态须明确租户与运行资源的所有权，授权、配额和子任务治理属于宿主责任。单用户本地部署可采用简化策略，无需预先建设完整 SaaS 控制台。

### 5. 可观察性保持可移植，并区分运行事实与遥测

**事实**：OpenTelemetry Collector 可接收、处理并导出 traces、metrics、logs 到不同后端，也可承担批处理、重试和敏感数据过滤；小规模场景允许直接导出而不部署 Collector。参见 [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/)。

**建议**：Peri 保留自己的领域事件，观测厂商作为可替换适配器。恢复依据的执行记录不应依赖可采样、可丢弃的 telemetry。运行治理应同时覆盖推理、工具环境和存储资源。

## 范围与限制

本次材料支持架构取舍，不构成 AWS、Cloudflare 或某工作流产品的选型推荐。未验证 Rust SDK、目标区域、价格、限额、冷启动及本仓库部署兼容性；这些应在确定部署形态后另行验证。本地与云端共用核心、分阶段落地，避免以“云原生”为由预先引入 Kubernetes、服务网格或自建通用工作流平台。
