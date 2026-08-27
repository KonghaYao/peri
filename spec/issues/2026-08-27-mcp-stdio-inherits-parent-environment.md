# MCP stdio 默认继承完整父进程环境

**状态**：Open
**优先级**：高
**类型**：安全 / 最小权限
**创建日期**：2026-08-27
**来源**：用户安全审查 + 静态代码核查

## 问题

本地 MCP stdio server 由 Perihelion 作为子进程启动。当前启动路径在配置的 `env` 上调用 `Command::envs(...)`，但不调用 `env_clear()`，因此子进程默认继承 Perihelion 进程的完整环境；`mcpServers.<name>.env` 仅追加或覆盖同名变量。

这使任一本地 stdio MCP server 都可能读取与其职责无关的环境变量，包括 API token、云凭据、代理配置及其他由启动终端或宿主进程传入的敏感信息。当前配置模型没有 `inheritEnv`、allowlist 或等价隔离机制，用户无法声明某个 server 仅接收必要变量。

## 当前实现事实

- `McpServerConfig.env` 表示显式传递给子进程的环境变量。
- `TransportConfig::Stdio` 将未配置的 `env` 规范化为空 map，但空 map 不代表空子进程环境。
- `spawn_stdio_transport` 通过共享 `shell_command` 创建进程命令，再调用 `envs` 注入配置变量；未清除继承环境。
- 配置中的 `command`、`args` 和 `env` value 支持 `${VAR}` 展开；插件 MCP 还会显式注入 `CLAUDE_PLUGIN_ROOT` 与 `CLAUDE_PLUGIN_DATA`。变量展开与子进程环境继承是两个不同机制，不应混为一谈。
- 现有测试覆盖配置到 `TransportConfig` 的 `env` 映射，但未通过真实子进程验证继承、覆盖和隔离语义。

当前有效语义为：

```text
stdio MCP 子进程环境
= Perihelion 父进程环境
+ server 显式 env（同名覆盖）
+ 插件上下文变量（适用时）
```

## 安全风险

- MCP server 的权限范围隐式扩大到宿主进程持有的全部环境凭据，不符合最小权限原则。
- 新增环境变量可能在未修改 MCP 配置的情况下自动暴露给既有 server，安全边界会随启动环境漂移。
- 第三方插件提供的 stdio MCP 配置可能获得与插件功能无关的 secret。
- 用户无法从配置中判断或限制实际传入子进程的环境集合。

本 issue 记录的是本地进程隔离问题，不涉及 Streamable HTTP header/OAuth secret 的传递规则，也不主张在诊断日志中输出环境变量名称或值。

## 期望改进方向

为 stdio MCP 定义明确、可配置且可测试的环境继承策略。具体配置名称由实现设计决定，但至少应支持关闭父环境继承，并在兼容性与安全默认值之间作出显式决策。

建议实现时评估：

1. 提供 `inheritEnv` 或等价策略，并明确旧配置的迁移行为。
2. 在隔离模式下保留启动命令所需的最小跨平台环境，例如经过审查的 `PATH`、系统目录或平台必需变量；不得无条件复制完整父环境。
3. 允许 server 显式 `env` 覆盖允许继承的同名变量。
4. 保持 `${VAR}` 配置展开能力，但不要因展开某个变量而隐式转发其他父环境变量。
5. 插件上下文变量继续按现有契约显式注入。
6. 错误、tracing 和 UI 状态不得记录 secret 值，也不应默认枚举潜在敏感变量名。

## 验收标准

- [ ] stdio MCP 配置可以显式选择不继承父进程环境。
- [ ] 隔离模式下，未显式允许或配置的父环境变量对子进程不可见。
- [ ] 显式 `env` 正确注入，并覆盖允许继承的同名变量。
- [ ] 默认策略、兼容性行为和跨平台最小环境有稳定契约说明。
- [ ] 插件 MCP 的 `CLAUDE_PLUGIN_ROOT` 与 `CLAUDE_PLUGIN_DATA` 在隔离模式下仍按契约可用。
- [ ] 真实子进程测试覆盖默认行为、隔离、显式注入、同名覆盖和缺失变量，且测试使用哨兵值而非真实 secret。
- [ ] Unix 与 Windows 启动路径均有验证，命令解析能力不因环境隔离而意外失效。
- [ ] 诊断信息、事件和日志不包含环境变量值。

## 相关代码

- `peri-acp-types/src/plugin.rs`：`McpServerConfig`
- `peri-middlewares/src/mcp/transport.rs`：`TransportConfig::Stdio`
- `peri-middlewares/src/mcp/client/transport.rs`：`spawn_stdio_transport`
- `peri-middlewares/src/mcp/config.rs`：配置合并、变量展开与插件上下文注入
- `peri-agent/src/agent/async_tasks/shell.rs`：`shell_command`
