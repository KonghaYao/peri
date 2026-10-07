# MCP 响应缓存策略

状态：现行设计。行为以实现和契约测试为准。

## 职责

MCP 响应缓存是客户端的可丢弃加速层，不是持久会话状态或资源提供方。typed 配置、来源合并与关闭优先规则由 `peri-config` 持有；Middleware loader 消费配置快照并适配插件与运行 overlay，`McpClientPool` 持有运行期不可变使用策略。配置 MCP 数据面只提供输入 I/O。MetaHarness 控制能力消费，不负责缓存策略。

总开关同时约束 Peri 的持久化响应缓存和 rmcp peer 的内存响应缓存。关闭不影响连接、工具权限、当前实例能力目录、技能注册表、输出归档、会话冻结数据、Apps relay、资源订阅或模型 prompt cache；也无法禁止远端 server 自身缓存。

## 配置

项目 `{cwd}/.mcp.json`：

```json
{
  "mcpCache": false,
  "mcpServers": {
    "server-a": { "url": "https://a.example.com/mcp" },
    "server-b": { "command": "./server-b" }
  }
}
```

用户级文件为配置数据面确定的全局 settings 路径，默认 `~/.peri/settings.json`，尊重已有路径重定向。支持顶层 `mcpCache` 或 `config.mcpCache`，同文件按字段 nested 优先；两处声明均须合法。仅包含 `{"mcpCache": false}`、没有 server map 的文件也生效。

环境变量：

```bash
PERI_MCP_CACHE=false cargo run -p peri-tui
```

`PERI_MCP_CACHE` 接受 `true/false`、`1/0`、`on/off`，忽略首尾空白与大小写；空字符串或其他值报配置错误，不静默开启、不回显输入值。环境变量通过独立配置数据面按名称读取：默认本地部署读取本机环境，远端配置部署读取配置 provider 的环境，不回落计算宿主环境。

| 输入 | 行为 |
| --- | --- |
| 未声明 | 本层不施加关闭限制 |
| `true` | 本层允许缓存，但仍受其他层和安全准入约束 |
| `false` | 本层关闭所有实例的响应缓存 |
| JSON `null` 或非 boolean | 配置错误，初始化失败 |

有效值为 `global.unwrap_or(true) && project.unwrap_or(true) && environment.unwrap_or(true)`。任一来源关闭就关闭；不存在高优先级 true 强制重新开启的语义。所有来源未声明时保留原有默认行为。

不提供 `mcpServers.<name>.cache`，插件不能修改总策略。server map 的来源优先级、去重、变量展开和 builtin overlay 不变。

## 生命周期与关闭闭包

```text
配置数据面提供文件/环境输入
  → peri-config 校验、关闭优先合并与 scoped snapshot
  → Middleware loader 消费快照、适配运行 overlay
  → initialize_config 安装不可变 pool 策略
  → peer 握手后、能力发现前配置 SDK 缓存
  → pool 缓存准入 / 实时 RPC
```

pending pool 的缓存准入 fail-closed。初始化在空 server map、Ready 或连接任务前安装策略；同一 pool 不能重新绑定不同策略。主会话、SubAgent 和 Workflow 共享该策略。

关闭覆盖工具描述、资源列表/模板/正文、技能列表与 legacy 技能正文响应。builtin、用户、插件、动态接入及 ACP 桥接实例均遵守同一总策略；初始连接、重连和 OAuth 完成均在能力请求前关闭 SDK 响应缓存。已有动态连接不允许持久缓存的安全规则保留。

关闭时绕过响应读取和新响应保存，相关 RPC 失败照常返回错误，不能从磁盘或 SDK 旧响应兜底。System MCP readiness 保留真实发现要求，不用历史持久缓存证明可用。

文件或环境修改不热变更既有 pool；配置源显式 reload 并重新取得快照后，新建 pool 消费新策略，旧 pool 仍绑定旧快照。bare 模式不读用户、插件或项目文件，但接受环境总开关。不提供修改既有 pool 策略的运行时 setter。

## 安全、历史文件与观测

允许缓存不代表所有响应均可缓存。认证上下文、动态连接、scope、TTL、版本、内容验证和 generation fencing 的既有约束仍生效；true 不能放宽它们。

关闭不自动清空历史文件、不改缓存 origin 或格式。通知驱动的失效维护继续执行，可能读写 epoch 元数据或移除失效条目，防止之后开启时命中已失效响应。因此开关不承诺缓存目录零 I/O。重新开启仍走现有版本与有效期验证，缓存损坏或不可用不能成为业务权威。

状态投影先表达配置关闭或尚未初始化，再考虑历史命中记录。TUI 区分“缓存关闭：配置”与认证上下文禁止持久化，不能把用户关闭显示为已认证。

## 验证入口

- `peri-middlewares/src/mcp/config/cache_policy_test.rs`：合法值、关闭优先矩阵、文件层合并、真实环境来源与 bare 路径；环境变更在子进程隔离。
- `peri-middlewares/src/mcp/client/cache_policy_test.rs`：预置缓存后获取 live 响应、不覆写 payload、错误不回落、SDK 缓存关闭、技能注册表仍更新、失效通知保留。
- `peri-middlewares/src/mcp/cache_policy_initialize_test.rs`：Ready 前安装策略及禁止重新开启。
- `mcp-packages/config/src/tests.rs`：环境来源经真实 MCP 通道读取，远端部署不回落计算宿主来源。

本领域规则归 [配置权威面](configuration-authority.md)。独立 pool 未注入快照时仍委托 core 解析，不能由此声称所有部署 pool 共用同一 revision；配置数据面不持有解析或合并权威。
