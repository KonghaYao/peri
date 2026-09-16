<!--
ROLE: 设计哲学与任务路由。工程细则 → docs/standards/。
-->

# CLAUDE.md — Perihelion

Perihelion 是终端 AI 编程助手：用户交付任务，Agent 推进工作，过程可理解、可介入，结果可核对。长期可维护性是设计目标。

## 设计哲学

- **任务完成与用户控制共同成立。** Agent 主动推进已授权的工作，需要人判断的取舍交还用户。界面优先呈现结果、阻塞和必要决策，过程按需展开；不让用户学习内部编排才能完成任务，不以自动化为由隐藏失败或削弱取消、审批能力。
- **模型负责判断，系统负责确定性。** 模型输出可以不确定，执行的身份、顺序、权限与终态应可验证。能由类型、协议和状态机保证的约束就在代码落实；不靠提示词弥补执行层缺口，不用重试或兜底把未知状态伪装成成功。
- **事实与视图分离。** 先确定事实的持有者，再派生模型上下文和界面视图。压缩、缓存和渲染围绕事实构建，不另建可独立漂移的真相。
- **边界稳定，能力可组合。** 用生命周期和职责决定状态归属，协议、执行、外部能力与界面各守边界。新能力优先接入已有扩展点；不为少写几行跨层直连，不为假想需求预建框架。
- **成本是设计输入。** 上下文、token、CPU、内存和用户注意力都有限。按需加载、渐进披露，让历史处理、后台任务和缓存的成本有界。性能取舍依据测量，不用数据失真、关键事件丢失或不可恢复状态换取速度。

理念不代表能力已实现。取舍先守住数据、权限与生命周期契约，再比较交付收益、理解成本和运行成本。

## 行事风格

- **像研究员一样判断，像工程师一样交付。** 区分观察、推断和假设，用代码、复现或实验形成结论。直说理由与局限，不营销、不补造数字，不把计划或命令启动当成完成。
- **在授权范围内主动闭环。** 常规选择依据仓库证据自行处理；改变用户目标、权限或不可逆结果的歧义及时澄清。不同意方案时说明代价并给出替代方案，不迎合，也不把日常判断推给用户。
- **改动要小而完整。** 沿因果链修复，覆盖受影响的调用方、契约和文档；不遮盖症状，不混入无关重构。必要重构以减少本次问题的复杂性为界；交付说明改动、验证证据和未验证项。

## 代码风格的取舍

- **显式表达领域语义。** 命名体现职责，类型表达身份、状态和错误；所有权与副作用沿调用链可见。避免用字符串约定、布尔组合和隐式共享状态承载关键语义。
- **降低理解成本。** 优先清楚的控制流、小接口和内聚实现；抽象应封装变化，减少调用方的认知负担。少量重复可接受，不为消除重复制造通用层、无语义转发或参数开关集合。
- **遵循邻近模式，解释必要例外。** 格式、依赖和惯用法沿用已有实践；注释解释不变量、取舍与非显然原因。局部模式违反契约时修正问题，不机械复制。细则见 [rust.md](docs/standards/rust.md)。

## 测试风格的取舍

- **测试保护行为和契约。** 按场景断言可观察结果：纯逻辑看输入输出，边界看序列化、错误、顺序与生命周期。内部重构不应迫使无关测试跟着改；不靠复制一遍实现来证明正确。
- **测试要能揭示目标故障。** 回归测试暴露原问题，并在修复后通过；覆盖相关失败、取消与边界情况。外部不确定性在边界替换，内部关键链路用真实实现；不以全套 mock 自洽推导生产可用。
- **验证力度随风险扩大。** 从目标测试开始，跨层验证完整链路，进程、恢复或平台承诺验证相应生命周期。测试要确定、隔离、可独立运行；不追求用例数量，不为样板代码制造负担。范围、门禁和证据见 [testing.md](docs/standards/testing.md)。

## 事实源与任务路由

信息优先级：代码/契约测试 > `docs/standards/` > 模块 `CLAUDE.md` > `docs/design/` > active spec > history。此顺序核对现行行为，不把缺陷当作目标；变更时同步事实源。

先读 [标准索引](docs/standards/index.md)。定位先查 `docs/code-index/`，按意图找主文件、核实入口符号，变更时同步索引。loader 不继承父目录，需显式读取模块指引。

| 任务 | 先读 |
| --- | --- |
| Agent loop、Compact、provider、session | `peri-agent/CLAUDE.md` + architecture/rust |
| ACP host、stdio、prompt、event、caps | `peri-acp/CLAUDE.md` + architecture/rust |
| Controller/Runtime、cancel、Langfuse | architecture/rust + 对应 code-index |
| MCP、plugin、skills、subagent、HITL、工具、LSP | `peri-middlewares/CLAUDE.md` + architecture/rust |
| Workflow | middleware guide + `docs/code-index/peri-workflow.md` |
| TUI | `peri-tui/CLAUDE.md` + tui/rust |
| E2E | `e2e/CLAUDE.md` + testing |
| 文档站 | `peri-cool/CLAUDE.md` + documentation |
| 历史学习 | `.claude/skills/learn-from-history/SKILL.md` |

简称均指 `docs/standards/`：architecture = `architecture-contracts.md`，其余同名。跨层边界、prompt、事件、工具、中间件顺序或安全变更先读 architecture；Git 操作读 `git.md`，指引维护读 `documentation.md`。

设计：`docs/design/README.md`；需求：`spec/issues/`；历史：`spec/global/problems.md`。主路径 `peri-tui → peri-acp → peri-agent::run_react_loop`，退出语义见 Agent 指引；workspace 以 `Cargo.toml` 为准。

## Workspace 命令

```bash
cargo build --workspace
cargo test -p <crate> --lib -- <test_name>
cargo test --workspace --doc
cargo run -p peri-tui
lefthook run pre-commit
cargo clippy --workspace --all-targets -- -D warnings
```

按变更范围选择命令；改 doc comment 跑 doc tests；E2E 命令见其指引。完成前按 `DOC-UPDATE-001` 核对路由。未经用户要求不 commit。
