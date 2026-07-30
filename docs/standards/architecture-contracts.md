# 跨模块架构契约

只记录跨模块稳定不变量。具体测试规范见 `docs/design/testing-standards.md`。

### ARC-BOUNDARY-001

- **Scope**：`peri-tui`、`peri-acp`、`peri-agent`。
- **Rule**：TUI 的用户交互主路径经 ACP transport 调用服务；不得从 TUI 直接驱动 `peri-agent` 或 `peri-middlewares` 的 Agent 运行时。TUI 可在启动和配置层复用相关 crate 的类型与初始化能力，Agent 执行入口仍保持在 ACP 会话路径。
- **Verify**：人工检查 TUI 的 prompt、cancel、session 等请求经 ACP client/transport，及 `peri-acp/src/session/executor_helpers.rs` 调用 `run_react_loop`。

### ARC-FROZEN-001

- **Scope**：会话、Prompt、SubAgent。
- **Rule**：会话创建时冻结日期、项目指引、skills 摘要和 system prompt；同一会话及其 SubAgent 复用冻结数据，禁止中途重新读取而改变 prompt 前缀。
- **Verify**：`cargo test -p peri-middlewares --lib frozen_claude_md`；人工检查 `FrozenSessionData::build` 与 SubAgent `with_frozen_data` 调用。

### ARC-EVENT-001

- **Scope**：`ExecutorEvent`、ACP 映射、TUI 通知。
- **Rule**：新增或变更事件必须覆盖完整链路：发射、ACP 映射/转发、能力门控（如适用）和 TUI 消费；终止事件必须使客户端离开 loading 状态。
- **Verify**：`cargo test -p peri-acp --lib mapper`；人工检查 `peri-acp/src/event/`、事件 sink 和 `peri-tui/src/kit/acp_notifier.rs` 的对应分支。现有测试不自动证明所有新增变体已覆盖。

### ARC-TOOLS-001

- **Scope**：工具注册、搜索与执行。
- **Rule**：工具以 `BaseTool::is_direct()` 自声明可见性；`true` 才直接进入 LLM tools，`false` 的工具只能由 `SearchExtraTools` 发现、`ExecuteExtraTool` 执行。包装层必须透传该 trait 语义。
- **Verify**：`cargo test -p peri-middlewares --lib core_tools`；检查 `peri-agent/src/tools/mod.rs`、`peri-middlewares/src/tool_search/` 与包装工具实现。

### ARC-SERIAL-001

- **Scope**：跨请求复用的 Prompt、工具注册与 provider payload。
- **Rule**：影响 prompt cache 的序列化顺序必须确定；不得直接依赖 `HashMap` 迭代顺序生成 tools 或其他缓存前缀。使用 `BTreeMap`、稳定排序或固定注册顺序，并保持包装层顺序不变。
- **Verify**：检查工具注册表及 provider payload 的收集路径；修改后运行相关工具注册测试，并比较相同输入的连续序列化结果。

### ARC-MIDDLEWARE-001

- **Scope**：生产中间件链。
- **Rule**：中间件顺序是行为契约，不得按名称、便利性或局部需求重排；链的唯一事实源是 ACP builder，详细任务入口为 `peri-middlewares/CLAUDE.md`。
- **Verify**：人工检查 `peri-acp/src/agent/builder.rs` 的 `MiddlewareChain` 构造顺序；修改该顺序时按 `docs/design/testing-standards.md` 增加或更新验证。

### ARC-SECRET-001

- **Scope**：配置加载、日志、错误、遥测、测试与提交。
- **Rule**：真实密钥、token、密码、私钥和连接串不得写入源码、fixture、日志、错误响应、遥测 payload 或版本库。运行时可通过环境变量、密钥管理或项目已支持且受本机权限保护的本地配置加载；输出和诊断只保留安全上下文。
- **Verify**：`git diff --check`；人工审阅变更中本地配置与环境注入、`tracing` 调用、错误格式化和测试 fixture，确认没有真实 secret 或完整认证信息。
