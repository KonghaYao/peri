# CLAUDE.md

Peri 是终端 AI 编程助手：用户交付任务，Agent 推进工作，过程可理解、可介入，结果可核对。长期可维护性是设计目标。

## 项目大目标

v4 目标是存算分离：持久状态、Agent 计算与工具执行环境独立，本地与云端共用核心，分阶段落地。存储后端可配置，Peri 实现持久化；计算核心减少重型依赖，实例可替换、执行可恢复，支持 serverless。Agent 决策、模型推理与工具执行可分开部署。迁移状态以代码、契约测试及 active spec 为准。

1. **Harness**：RCRA（Receive → Compact → Reason → Act）循环执行，hook 扩展生命周期，Middleware 承载业务能力。
2. **Sessions**：存储会话、消息及执行恢复状态，后端可替换；会话、任务运行与计算实例具有独立生命周期。
3. **Resources**：文件系统工具、Skill、Cron 等能力经 MCP Middleware 接入；工作区与工具环境可独立于计算实例驻留。
4. **Orchestration**：基于同构 Agent，管理 Subagent、Multitask 与 Workflow 的任务关系、协调、等待和恢复；Middleware 提供接入，编排生命周期不绑定某个活跃 Harness 实例。
5. **Endpoint**：ACP 是统一出口协议，stdio 是本地传输方式；传输层可自定义，客户端复用同一业务语义。

**内部依赖走 MCP，外部出口走 ACP**：依赖按能力消费方向定义，与部署位置无关；MCP 能力边界不强制对应独立进程，部署隔离按信任边界和生命周期确定。

## 核心工程原则 v1.5

1. **架构与领域优先**：编码前明确目标、领域边界、职责、依赖方向和数据流。复用型抽象等第二个真实用例再提取；协议隔离、依赖反转和确定的可替换边界可在首个用例建立接口，须说明当前需要。
2. **模块封装复杂度**：接口精简、稳定，文件按职责拆分；规模限制及验证遵循 `STD-SIZE-001`。
3. **边界与数据流清晰**：协议、领域、持久化与视图模型各守边界；在边界校验和转换，避免跨层共享可变状态。
4. **保留维护上下文**：记录决策原因、影响与风险，临时方案注明移除条件；技术债务关联任务，关键决策同步文档，不留无上下文的 `TODO`。
5. **删除优于兼容**：内部重构删除过时实现，不新增兼容层、deprecated shim 或双写；对外兼容义务按协议评估。
6. **业务规则单一权威**：同一领域规则只维护一份实现；允许多个存储后端和协议 adapter 封装适配差异，共用业务规则与契约。
7. **优先验证完整行为**：先验证用户可观察的行为，再按风险补齐回归、失败和生命周期测试；遵循 `testing.md`。

## 行事风格

- **像研究员一样判断，像工程师一样交付。** 区分观察、推断和假设，以代码、复现或实验支持结论；说明局限，不补造数字，不把命令启动当成完成。
- **在授权范围内主动闭环。** 依据仓库证据处理常规选择；澄清目标、权限或不可逆结果的歧义。异议须说明代价和替代方案。
- **改动要小而完整。** 沿因果链覆盖调用方、契约和文档；重构限于本次问题，交付说明改动、验证证据和未验证项。

## 代码风格的取舍

- **显式表达领域语义。** 命名与类型表达职责、身份、状态和错误；所有权和副作用可见，避免字符串约定与隐式共享。
- **降低理解成本。** 控制流清楚、接口精简；允许少量结构重复，避免制造通用层或参数开关集合。
- **遵循邻近模式。** 沿用格式和惯用法，注释解释不变量与取舍；违反契约的模式应修正。细则见 [rust.md](docs/standards/rust.md)。

## 测试风格的取舍

- **保护行为和契约。** 按场景断言输入输出、序列化、错误与顺序；避免复制实现或让无关测试耦合内部重构。
- **揭示目标故障。** 回归测试须暴露原问题，修复后通过；外部不确定性在边界替换，内部关键链路用真实实现。
- **随风险扩大验证。** 覆盖相关失败、取消、进程恢复和平台生命周期；测试应确定、隔离、可独立运行。范围与证据见 [testing.md](docs/standards/testing.md)。

## 事实源与任务路由

先读 [标准索引](docs/standards/index.md)，区分现状与目标。按 `docs/code-index/` 核实入口、同步变更。Peri loader 不继承父目录，须显式读取模块指引。

| 任务 | 先读 |
| --- | --- |
| Agent loop、Compact、provider、session | `peri-agent/CLAUDE.md` + architecture/rust |
| ACP host、stdio、prompt、event、caps | `peri-acp/CLAUDE.md` + architecture/rust |
| Controller/Runtime、cancel、Langfuse | architecture/rust + 对应 code-index |
| MCP（含内置实例）、plugin、skills、subagent、HITL、工具、LSP | middlewares 与 `mcp-packages/CLAUDE.md` + architecture/rust |
| Workflow | middleware guide + `docs/code-index/peri-workflow.md` |
| TUI | `peri-tui/CLAUDE.md` + tui/rust |
| E2E | `e2e/CLAUDE.md` + testing |
| 文档站 | `peri-cool/CLAUDE.md` + documentation |
| 历史学习 | `.claude/skills/learn-from-history/SKILL.md` |

简称指同名标准文件，architecture 指 `architecture-contracts.md`；跨层、prompt、事件、工具、链序或安全变更读 architecture，Git 操作读 `git.md`，指引维护读 `documentation.md`。

设计：`docs/design/README.md`；需求：`spec/issues/`；历史：`spec/global/problems.md`。主路径 `peri-tui → peri-acp → peri-agent::run_react_loop`；退出语义查 Agent 指引，workspace 查 `Cargo.toml`。

## Workspace 命令

```bash
cargo build --workspace
cargo test -p <crate> --lib -- <test_name>
cargo test --workspace --doc
cargo run -p peri-tui
lefthook run pre-commit
cargo clippy --workspace --all-targets -- -D warnings
```

按范围选择命令；改 doc comment 跑 doc tests；E2E 查其指引。交付前按 `DOC-UPDATE-001` 核对路由。未经要求不 commit。
