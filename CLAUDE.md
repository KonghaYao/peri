# CLAUDE.md

Peri 是终端 AI 编程助手：用户交付任务，Agent 推进工作，过程可理解、可介入，结果可核对。长期可维护性是设计目标。

## 项目大目标

v4 分阶段推进存算分离。阶段状态以代码、契约测试及 active spec 为准。

- **单机本地：已完成。** 本地 Agent、会话与工具形成可用闭环。
- **单机服务化：未完成。** TS SDK 负责入口、进程与跨主 Agent 管理；Peri 负责 ACP 后端、Agent 执行、Store 直连与 MCP Client。重启后保留历史会话加载与续聊，不恢复旧执行。
- **集群化 / Serverless 化：未完成。** 推进多实例协调与接管、工具环境独立驻留及云端部署；计算核心减重、支持实例替换，决策/推理/工具执行可分开部署。

1. **Harness**：RCRA（Receive → Compact → Reason → Act）循环执行，hook 扩展生命周期，Middleware 承载业务能力。
2. **Sessions**：存储会话、消息、配置及冻结上下文，不持久化 Agent 执行恢复状态；后端可替换；会话、任务运行与计算实例具有独立生命周期。
3. **Resources**：文件系统工具、Skill、Cron 等能力经 MCP Middleware 接入；工作区与工具环境可独立于计算实例驻留。
4. **Orchestration**：基于同构 Agent，管理 Subagent、Multitask 与 Workflow 的任务关系、协调与当前运行期等待；不恢复旧执行。Middleware 提供接入，编排生命周期不绑定某个活跃 Harness 实例。
5. **Endpoint**：ACP 是统一出口协议，stdio 是本地传输方式；传输层可自定义，客户端复用同一业务语义。

**内部依赖走 MCP，外部出口走 ACP**：依赖按能力消费方向定义，与部署位置无关；MCP 能力边界不强制对应独立进程，部署隔离按信任边界和生命周期确定。
**减少文件系统依赖，推崇网络依赖**：文件系统依赖转移到 mcp-packages 层以下，不可外溢。

## 核心工程原则 v1.7

架构设计关注点

1. **架构与领域优先**：计划阶段应以理想架构为目标，明确业务目标、领域边界、模块职责、依赖方向和数据流，形成符合领域规律、面向长期维护且可持续演进的设计后再进入编码；不得以短期实现便利牺牲整体设计。
2. **追求优雅的代码模块**：模块应高内聚、低耦合，通过精简且稳定的接口封装内部复杂度，使职责、命名、依赖和扩展方式清晰自然；代码按单一职责拆分，单个文件不得超过 1000 行，接近上限时应优先重构模块边界。
3. **业务规则单一权威，保持边界与数据流清晰**：协议模型、领域模型、持久化模型和视图模型不得相互泄漏；数据必须在边界处完成校验和独立转换，避免跨层共享可变状态。
4. **设计架构强调灵活性，而非强耦合**：同一领域规则只维护一份设计 interface，使用多个 adapter 封装适配差异，共用业务规则与契约。
5. **数据库表结构须经用户明确批准**：设计变更需要说明表及结构的合理性，由用户裁决；减少不必要的字段设计，禁用过渡表、字段及索引。
6. **异常处理：暴露异常优于简单掩盖**：默认的错误处理方式都是抛出原始错误，不主动修改内容更不可替换异常，由上层处理，并在日志中留下记录。

开发关注点

1. **面向并发与故障设计**：后端应主动考虑幂等性、竞态、事务边界、超时、取消、重试、背压和资源释放；不得通过无边界重试、吞错或隐式共享状态掩盖问题。
2. **复用已有组件，而非自己开发**：严格复用项目中已有组件及模块，在需要新增模块或者组件时，考虑开源生态中的常用解决方案，向用户提出，准许后执行。
3. **优先验证完整行为，保障完整前端体验**：先验证用户可观察的行为，再按风险补齐回归、失败和生命周期测试。前端应控制渲染成本、异步状态和并发请求，保持清晰的 UI 结构；用户流程必须覆盖加载、空状态、错误、重试、反馈。
4. **克制地为未来维护者保留上下文**：非显然的取舍在注释里写一句「为什么」，长背景引用权威文档；注释不写代码表面行为、改动历史或备选方案。技术债务应关联可追踪任务，禁止留下缺少上下文的 `TODO`。单条注释一般 2 句话内写完。
5. **手术刀式修复，如无必要，勿增开发量**：控制修复 bug 的影响文件数量，只修改必要的，可选的问题，以推荐的形式告知用户。

测试关注点

1. **测试顺序是快速验证为先，集成测试为后**：先测局部，后测全局。
2. **确保变更可验证、可观测**：每项改动都应行为可测试、运行状态可观测、故障可定位，并兼顾向后兼容路径；错误与日志必须保留诊断上下文，但不得泄露敏感信息。
3. **修复一类问题，而不是一个问题**：当反馈问题时，首先应该思考这类型的原因触发的位置，快速定位错误方向，手术刀式解决问题。
4. **用户反馈的问题要固化**：缺陷补回归测试；设计或契约变化同步到权威文档内。不把反馈过程和改动历史写进注释。

## 行事风格

- **像研究员一样判断，像工程师一样交付。** 区分观察、推断和假设，以代码、复现或实验支持结论；说明局限，不补造数字，不把命令启动当成完成。
- 汇报风格：用户有阅读障碍，减少字数，多用 order list 结构化表达。

## 事实源与任务路由

**代码定位首选 `codebase-index`**：按路由锁定 crate，读 1–2 个 `docs/code-index/` 文件；已知目标直接 `rg`。先读 [标准索引](docs/standards/index.md) 区分现状与目标。Peri loader 不继承父目录，须按路由显式读取对应文档。

| 任务 | 先读 |
| --- | --- |
| Agent loop、Compact、provider、session | `docs/code-index/peri-agent.md` + architecture/rust |
| ACP host、stdio、prompt、event、caps | `docs/code-index/peri-acp.md` + architecture/rust |
| Controller/Runtime、cancel、Langfuse | architecture/rust + 对应 code-index |
| MCP（含内置实例）、plugin、skills、subagent、HITL、工具 | `docs/code-index/peri-middlewares.md`、`docs/code-index/mcp-packages.md` + architecture/rust |
| Workflow | middleware guide + `docs/code-index/peri-workflow.md` |
| TUI | `docs/code-index/peri-tui.md` + tui/rust；集成用例 `docs/verification/tui-integration-cases/` |
| E2E | `docs/standards/testing.md` |
| 交付验证 | `scripts/check-changed.sh`、`scripts/verify-async.sh`（开工先读 read） |
| 文档站 | `docs/standards/documentation.md` |
| 历史学习 | `.claude/skills/learn-from-history/SKILL.md` |

简称指同名标准，architecture 指 `architecture-contracts.md`；跨层、prompt、事件、工具、链序或安全变更读 architecture，Git 操作读 `git.md`，指引维护读 `documentation.md`。

设计：`docs/design/README.md`；需求：`spec/issues/`；历史：`spec/global/problems.md`。主路径 `peri-tui → peri-acp → peri-agent::run_react_loop`；退出语义查 `docs/code-index/peri-agent.md`，workspace 查 `Cargo.toml`。

## Workspace 命令

```bash
./scripts/cargo-rmcp-patched.sh build --locked --workspace
./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib -- <test_name>
lefthook run pre-commit
```

其余命令见 `patches/README.md`。

按范围选择命令；改 doc comment 跑 doc tests；E2E 查其指引。交付前按 `DOC-UPDATE-001` 核对路由。未经要求不 commit。
