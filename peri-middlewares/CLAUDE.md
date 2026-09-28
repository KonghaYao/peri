# peri-middlewares

## Scope

`peri-middlewares` 承载提示词、工具、插件、审批与子 Agent 能力。生产链蓝本在 `../peri-agent/src/session/factory.rs::production_blueprint`，本 crate 的 `src/assembly.rs` 实现装配；链顺序和条件以两者为准。builtin MCP 实例在 `src/mcp/builtin/`，不是 middleware 槽位，关闭走 `BUILTIN_INSTANCE_POLICY_KEYS`；`ChainSlot::Lsp` 只挂文档同步。workspace 实例包装既有文件/终端实现；其 `system_mcp_tools` 选中工具以原名注入模型面，未选中工具仍走带前缀的 deferred 路径。


自动 compact 属于 Agent 执行阶段，参见 `../peri-agent/CLAUDE.md`。

## 数据流/架构

`SessionContext/config → Agent 层 session 工厂（`build_middleware_chain` 唯一触发点 + `production_blueprint` 链序蓝本）→ assembly.rs 槽位构造 → MiddlewareChain → prompt_contribution + collect_tools → Agent stage`。

- 插件提供 skill roots、agent dirs、hook groups 与 MCP 配置，由对应中间件消费。
- MCP 配置按全局 `~/.peri/settings.json`、插件、项目 `{cwd}/.mcp.json` 合并；工具与资源仅在 pool 可用时注册。
- Skills 按用户目录、配置的 `skillsDir`、项目目录、插件和内置来源的优先级搜索；目录含 `SKILL.md` 即为叶子，不再下钻。同名按来源顺序优先。
- SubAgent 从父工具、冻结上下文、取消策略与事件处理器派生执行上下文；具体 agent 定义和内置 agent 请直接查 `src/subagent/` 与项目 `.claude/agents/`，如需举例只使用 `explorer`。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 生产链顺序、条件注册、跨 crate 装配 | `../peri-agent/src/session/factory.rs`（蓝本）与 `src/assembly.rs`（槽位构造） |
| hook 状态能力与回写（ARC-MIDDLEWARE-CAPABILITY-001） | `../docs/standards/architecture-contracts.md`；`../peri-agent/src/middleware/capabilities.rs` |
| MCP 合并、server/tool bridge | `src/mcp/` |
| Plugin manifest、commands、agents、MCP 回退 | `src/plugin/` |
| Hook 事件与执行器 | `src/hooks/` |
| Skills 扫描、预加载、工具 | `src/skills/`、`src/skills/tools.rs` |
| SubAgent、后台任务、取消和事件 | `src/subagent/` |
| HITL 权限与审批 | `src/hitl/` |
| Workflow、工具搜索、LSP 文档同步 | `src/workflow/`、`src/tool_search/`、`src/lsp/middleware.rs`（`LspSyncMiddleware`，无工具） |
| Artifact 公开上传工具、builtin MCP 实例 | `src/mcp/builtin/`（`web` / `artifact` / `cron` / `lsp` / `workspace` 的 handler、`dispatch.rs`、`context.rs`、代监督者 `runtime.rs`）；`src/artifact/` |
| 文件 / 终端工具与 Todo、Cron 调度 | 工具实现 `src/tools/filesystem/` 与 `src/middleware/terminal.rs`（`BashTool`）；工具面 `src/mcp/builtin/workspace.rs`（7 项全 direct）；`src/middleware/todo.rs`；cron 只剩 `src/cron/` 的 scheduler 与 `tools.rs` |

## 稳定不变量

- **链顺序**：只能在 Agent 层 session 工厂的链序蓝本（`production_blueprint`）与 `src/assembly.rs` 槽位构造中判断与修改生产顺序；不得按名称或局部便利重排。
- **MCP**：保留三层合并、内容去重和插件命名空间；配置来源或工具注册变更必须同时检查 pool、资源与 bridge 路径。init/OAuth/reconnect/subscription 任务由 deployment-held non-Clone `McpTaskOwner` 持有，并实现契约层 `McpTaskOwnerPort` 供 ACP boxed 注入；pool 只持 weak spawner。正常关闭顺序固定为 pool begin-close → owner abort/join → pool service close。Pool service close 由 pool-held 单一 transaction 持有，waiter 取消/并发/重试必须观察同一 `McpPoolShutdownReport`；cleanup timeout 保持 `Closing`，不得发布 `Closed`（ARC-HOST-SHUTDOWN-001）。
- **MCP 调用恢复**：仅 workspace builtin 保留原生期限，其他 builtin 与外部 MCP 发送/响应共用 120 秒。超时或 drop 发有界取消，handler 监听 request token；完成证据仍属工具/会话 owner。仅投影安全原因及工具生成的任务、日志、草稿引用，不透传任意错误。入口和真实边界测试见 `../docs/code-index/peri-middlewares.md`。blocking 搜索只协作取消，不承诺 drop 时已 join。
- **MCP over ACP**：client 在会话 setup 声明的 `type: "acp"` server 由 `src/mcp/acp/`（`AcpMcpService`）承载。`attach` 只登记并后台建连（会话建立不等连接），失败留在池状态面（`ConfigSource::Acp` 条目）而不回抛；连接按声明它的会话归属，工具桥接、发现面与状态面必须按 `is_visible_to_session` 过滤，不得跨会话泄漏；`mcp/message` 内层错误码原样透传（lifecycle 依赖方法级错误码）；会话结束在池关闭前 `close_session`（幂等，`mcp/disconnect` 有上界）（ARC-MCP-ACP-001）。
- **System MCP 启动准入**：`system_mcp=true` 的 server 须在首个 Reason 前完成 transport、initialize、能力协商与真实 `tools/list`（空数组成功，Err 不是发现证据），由 `before_react_start` 闸门阻断未就绪 loop。失败/timeout 返回类型化错误，不发布 ready；取消按中断分类。`system_mcp_tools` 按所属 server 原始工具名精确匹配；仅选中项 direct，未选中项维持原发现路径，`[]` 仅要求 ready。选中项以原始工具名进入模型面，普通 MCP 与 deferred 工具仍使用 `mcp__<server>__<tool>`。模型名冲突按确定准入顺序 first-wins，记录 warning 并跳过后续项；真实必需工具缺失仍失败。权限使用绑定来源身份，不凭裸名授予 builtin 权限。readiness 不绕过 Permission/HITL/事件/cancel。builtin 同构：web/artifact/workspace 选中各自 direct 集，cron/lsp 零提升。默认层注入，加载期拒绝 `disabled + system_mcp`。
- **插件 MCP 配置严格路径**：`load_enabled_plugins_for_mcp` 对非法 MCP 配置直接失败、不降级为空配置；宽容展示 API（`load_enabled_plugins_aggregated` 等）行为保持不变。
- **Plugin manifest**：`commands` 条目兼容字符串路径与对象；字符串是相对插件根目录的路径。agents 未声明时仍保留约定目录回退。不要把路径条目当作名称。
- **Skills**：扫描必须保持根优先级、递归边界、符号链接防环、叶子语义和同名覆盖规则；插件 skill root 通过既有扩展点传入。
- **SubAgent**：同一会话的子 Agent 复用冻结的项目指引、skills 与 system prompt；同步子任务继承父取消，独立后台任务使用自身取消策略。`Agent(resume_thread_id, prompt)` 优先向当前会话的 live 后台执行投递 Info（非空 prompt），返回 `action: send / status: queued`；无 live 接收者且磁盘仍 active 时拒绝，非 active 才恢复并返回 `action: resume`。Info 不中断或唤醒模型，queued 不代表已读。事件必须按 `source_agent_id` 归属，新增事件同时检查父/子边界、完成和取消路径。
- **HITL**：审批以解析后的 effective tool name 为准，包装、搜索或代理工具不得绕过审批；权限模式与 broker 的选择必须保持一致。
- **工具可见性**：direct/deferred 语义由工具声明和工具搜索路径共同保证，包装层不得改变其可见性。

## 目标命令

从仓库根目录执行：

```bash
cargo build -p peri-middlewares
cargo test -p peri-middlewares --lib
cargo test -p peri-middlewares --lib -- mcp::task_scope
cargo test -p peri-middlewares --lib -- mcp::acp
cargo test -p peri-acp --lib
```

## 按需引用 / Verify

- 链、工具注册与条件中间件：`../peri-agent/src/session/factory.rs` 与 `src/assembly.rs`；同时遵守 `../docs/standards/architecture-contracts.md` 的 `ARC-MIDDLEWARE-001`、`ARC-TOOLS-001`、`ARC-FROZEN-001`。
- Plugin/MCP 或 Skills 改动：阅读目标模块的实现与测试后运行对应 `cargo test -p peri-middlewares --lib <过滤词>`。
- System MCP 准入 / 工具注入改动：`cargo test -p peri-middlewares --lib -- mcp::system_tools`、`-- mcp::client::readiness`、`-- mcp::middleware`；契约测试 `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` 与 `--test mcp_isolation_contract -- --test-threads=1`。
- SubAgent 或 HITL 改动：覆盖冻结数据、取消、事件归属及 effective tool name 的相关测试。
- 所有修改完成后运行 `git diff --check`；不得在日志、错误或测试 fixture 中写入密钥、token、密码或连接串。
