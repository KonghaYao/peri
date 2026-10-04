# 环境变量使用清理

状态：进行中。此 issue 记录环境变量使用的审计、清理与验收；[环境变量控制表](../../docs/standards/environment-variables.md)是主入口，[TUI 专属表](../../docs/standards/tui-environment-variables.md)单独维护 TUI 明细。下表的“当前事实”记录建单时基线；已实施行为以代码、契约测试和权威表为准。本 issue 不将环境变量一律迁入 `peri-config`。

## 已实施范围

- 模型配置不再使用 `ANTHROPIC_*` / `OPENAI_*` fallback；`MODEL_PROVIDER` 与 `MODEL_TYPE` 成对选择已配置 provider ID 与档位，显式会话模型选择仍可覆盖初始选择。
- Compact 在 Workflow 路径加载用户 `config.compact`；Agent loop 不再重复读取禁用环境变量。
- `PERI_MCP_BUILTIN` 保留；`PERI_MCP_APPS` 的客户端启动方式进入权威表；`PERI_WRITE_DRAFT` 生产开关移除，草稿默认开启。
- Artifact MCP 接收实例 `env`，由实例解释 URL/token；宿主启动和重连只透传配置。TUI 变量独立成表；Web PTY 已退役，相关变量入口已移除。

后续仍需完成全量环境来源/隔离审计；下方未勾选的验收项继续跟踪。

## 背景与目标

环境变量目前由配置来源、宿主装配、Agent、TUI、MCP、子进程运行时分别读取。一些变量是用户控制项，一些是 OS/终端输入、执行凭据、动态占位符或 Peri 向子进程注入的值；它们的所有权和生效时点不同。先前缺少统一清单，容易出现重复读取、同名不同义、`0` 仍触发的存在性开关、配置环境与计算宿主环境混淆，以及文档继续宣传已退役变量。

目标是让每个具名控制项有清楚的消费边界、取值规则、默认行为和验证，并在改变行为时同步权威表。清理应减少无意的进程全局读取和多处规则实现，同时保留 OS 能力探测、运行环境与按名称提供的凭据所需的专属入口。

## 用户裁决与目标边界

| 对象 | 当前事实 | 清理目标 |
| --- | --- | --- |
| `ANTHROPIC_*`、`OPENAI_*` | `peri-config/src/provider.rs` 仍按厂商变量提供 provider/model/key/base URL 环境 fallback；TUI 也会检查对应 key 是否存在。 | 全部列为**待删除的 Peri 模型环境控制项**；删除消费、采集、测试及现行文档用法。模型凭据和 endpoint 从配置的 provider 取得，不把厂商 key 移入新环境变量。外部 MCP/子进程独立使用同名变量的情形按其环境边界另行评估。 |
| `MODEL_PROVIDER` + 新增 `MODEL_TYPE` | 现有 `MODEL_PROVIDER` 只把 `anthropic` 解释成协议类型，`MODEL_TYPE` 尚未实现。 | `MODEL_PROVIDER` 改为 settings 中的 provider ID，`MODEL_TYPE` 为 `fable`/`opus`/`sonnet`/`haiku` 档位；两者一起选定该 provider 配置下对应档位的模型。未指定时沿用 settings 的 active profile；只有一个变量、未知 ID/档位、档位模型缺失或凭据缺失时应明确处理并测试，不静默选到另一 provider。新环境选择与用户显式 session 模型选择的优先级须在实施前由现有配置契约确认并锁定测试。 |
| Compact | `config.compact` 已有模块配置；主 prompt、手动 `/compact` 会叠加 env，Agent 执行链再直接读取 `DISABLE_*`；一条 Workflow 构造路径只用默认 Compact 配置。 | **一个修复项**：各执行路径统一消费装配好的 `CompactConfig`，补上 Workflow 的用户配置并去掉 Agent 层重复环境读取。三项环境变量是否继续作为装配时覆盖，不在本修复中擅自改动其现行语义。 |
| `PERI_MCP_BUILTIN` | 全局注入闸门：`off`/`0` 会阻止内建 `web`、`artifact`、`cron`、`lsp`、`workspace` MCP 实例默认注入；`workspace` 关闭会影响文件/终端、技能和项目指引资源，`--bare` 也受影响。 | **保留**现有开关及行为，不再列为待删除或待裁决项。 |
| `PERI_MCP_APPS` | stdio ACP 启动时按变量存在性选择 Apps profile；Peri 只提供 relay，下游客户端负责 Apps UI。 | 保留。支持 MCP Apps 的客户端启动 Peri 时携带该变量；文档和客户端集成示例写明启动时设置，不能依赖连接建立后再设置。空值也启用的现行语义保持。 |
| `PERI_WRITE_DRAFT` | Workspace Write/SandboxWrite 构造时直接读取，默认开启草稿，`0`/`false` 可关闭。 | **删除此环境开关**及相关解析/测试/文档；草稿功能保持默认开启，除非另有产品配置裁决。 |
| `PERI_ARTIFACTS_URL`、`PERI_ARTIFACTS_TOKEN` | 当前由**进程内 Artifact MCP** 的 `ArtifactTool::new` 直接读取宿主进程环境；尚无显式 MCP 透传边界。 | 改为经 MCP 实例环境/配置**透传**给 Artifact MCP，由 MCP 自行解释 URL 与 token；Peri 宿主和统一配置面不特殊解析、保存或记录值。进程内实例须有显式实例输入，不能把共享进程环境直读称为透传。 |
| TUI 专属变量 | 终端能力、渲染/交互诊断等多项读环境。 | 权威明细独立放在[TUI 表](../../docs/standards/tui-environment-variables.md)，主表只保留入口；TUI 后续清理在自身边界验收。 |
| Web PTY 的 `HOST`、`PORT`、`SHELL`、`CWD`、`CMD` | 建单时由 Web PTY 消费；现已随产品退役删除。 | 通用 OS `SHELL` 等若仍被其他组件使用，不按名字全局清除。 |

Artifact 与 builtin 的结论来自当前构造/注入调用链的静态检查；尚未运行隔离部署实验。Compact 专项由 subagent 静态检查，未运行测试。`MODEL_PROVIDER` 与 `MODEL_TYPE` 的新语义由用户确认，不能把上表目标写成当前已实现行为。

## 已确认的现状入口

| 领域 | 当前入口与待核对点 |
| --- | --- |
| Provider / Langfuse / MCP 配置 | `peri-config/src/{provider,observability,mcp,source}.rs` 经 configuration MCP 采集具名环境；settings、环境 fallback 与 MCP cache 的优先级各不同。 |
| Agent compact | `peri-config/src/app.rs` 已持有 `config.compact`；`peri-acp-types/src/compact.rs` 仍解释三个环境变量，`peri-agent/src/agent/stages/compact.rs` 与 `session/exec/executor.rs` 再直读关闭开关；`peri-acp/src/host/workflow_agent.rs` 一条路径只用默认值。 |
| ACP / 会话 | `peri-acp/src/broker/transport_broker.rs` 读取 AskUser 超时；`peri-acp/src/host/stdio/mod.rs` 按变量存在性启用 Apps；`peri-resources/src/sessions/machine.rs` 初始化机器 ID。核对读取时点和生命周期归属。 |
| TUI | `peri-tui/src/main.rs` 从 settings 注入未设置的环境值；TUI 专属明细已独立列出。核对启动冻结、运行时读取和 CLI 覆盖关系。 |
| MCP / 工具 / 子进程 | `peri-middlewares/src/mcp/`、`mcp-packages/`、`peri-js-runtime/src/artifact.rs`、`peri-workflow/src/runner/artifact.rs` 读取凭据、占位符、PATH 与后备开关；区分来源数据面和执行环境，并复核传给子进程的 allowlist。 |
| 文档与测试 | `docs/standards/environment-variables.md` 已建立；其他文档可能保留局部说明。环境变量是进程全局状态，测试读写侧须隔离，不能只给写侧加锁。 |

以上是静态入口，不等于已经证明存在生产行为错误。具体缺陷须通过调用链或测试确认后修复；不要仅凭 `std::env` 搜索命中决定删除。

## What to build

1. **模型选择迁移**：删除厂商模型环境 fallback、TUI 厂商 key 环境探测及不再需要的来源采集；加入 `MODEL_TYPE`，让 `MODEL_PROVIDER` 按 provider ID 与档位组合解析模型，凭据/endpoint 仍来自 provider 配置。覆盖缺失、非法、多个 provider、settings 与显式会话选择的优先级。
2. **Compact 单项修复**：主 prompt、手动 `/compact`、Workflow、恢复与 Agent loop 共用已装配 `CompactConfig`；修复 Workflow 的默认值旁路与 Agent 层重复环境读取。现有 env 覆盖如需保留，只在装配边界应用一次。
3. **MCP 专属环境**：保留 `PERI_MCP_BUILTIN` 与 `PERI_MCP_APPS` 的现行开关语义，补 Apps 客户端启动说明；移除 `PERI_WRITE_DRAFT`；通过 MCP 实例环境/配置透传 Artifact URL/token，由 Artifact MCP 消费，覆盖本地进程内与外部 MCP 装配边界。
4. **逐项审计与隔离**：以主表和 TUI 表为基线，扫描具名、常量间接及动态按名称读取；记录来源、生效时点和子进程传播。检查 `config.env` / Claude settings 注入、配置 MCP 与计算宿主环境隔离、会话冻结及并发隔离。保留 OS 目录、PATH、终端能力与按名称指定的执行凭据专属入口。
5. **文档同步**：TUI 变量在专属表维护；其他文档只解释相关功能并链接权威表，目标落地后同步 code-index、模块指引和表项。

## 验收标准

- [ ] `ANTHROPIC_*` / `OPENAI_*` 不再作为 Peri 模型配置入口；`MODEL_PROVIDER` + `MODEL_TYPE` 按已确认的 provider ID / 档位组合选模型，凭据和 endpoint 来自对应 settings provider。缺失、非法和优先级场景均有测试。
- [ ] Compact 单项修复完成：自动、手动、Workflow 和恢复路径消费已装配的 `CompactConfig`，Agent 不再重复读取环境；用户配置与现有环境覆盖的优先级有定向测试。
- [ ] `PERI_MCP_BUILTIN` 保留且现行开/关能力不退化；支持 Apps 的客户端启动示例携带 `PERI_MCP_APPS`，relay 开/关与客户端能力边界有验证。
- [ ] `PERI_WRITE_DRAFT` 的读取/解析/文档已删除，默认草稿行为仍验证；Artifact URL/token 经 MCP 实例环境/配置透传后由 Artifact MCP 消费，Peri 宿主不解释、不记录凭据，进程内和外部实例都覆盖。
- [ ] 生产代码中每个仍有效的具名控制变量均在主表或 TUI 表有单独条目，现行行为与表一致。
- [ ] 同一控制规则没有跨模块的相互矛盾实现；需要重复读取的地方说明生命周期原因，并由测试证明结果一致。
- [ ] 配置来源环境与计算宿主环境的差异有针对性验证；MCP cache、Langfuse 的既有优先级保持符合契约。
- [ ] 具名开关的缺失、空值、合法/非法值及优先级有定向测试；进程级环境测试隔离读写双方，不依赖开发者的真实 HOME、密钥或外部网络。
- [ ] 子进程环境按对应信任边界传递；PTC、动态 MCP、hook 等路径没有无意继承无关凭据，按名称指定的远端凭据不会把值写入日志或文档。
- [ ] 受影响用户文档没有把已退役变量写成可用开关；主表、TUI 表、标准索引和 code-index 路由保持一致。按改动范围运行目标测试与 `git diff --check`，记录实际结果及未覆盖平台。

## 关联任务与边界

- [配置权威面任务](2026-10-01-configuration-authority.md)仍负责 scoped snapshot、consumer 接线和配置来源；本 issue 只处理环境变量使用及其交界，避免重复实现完整配置系统。
- [Session ID / 机器环境任务](2026-09-30-session-id-environment-core-change.md)负责机器 ID 与会话环境分区；本 issue 只验证 `PERI_MACHINE_ID` 的读取及文档语义，不重开恢复契约。
- 安全与进程环境边界遵循 [ARC-SECRET-001](../../docs/standards/architecture-contracts.md)，测试隔离遵循 [testing.md](../../docs/standards/testing.md)。
