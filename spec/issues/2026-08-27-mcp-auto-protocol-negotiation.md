# MCP 客户端缺省未启用跨时代自动版本协商

**状态**：Open
**优先级**：高
**类型**：缺陷 / 协议兼容性
**创建日期**：2026-08-27
**来源**：用户报告 + MCP 官方规范核查 + rmcp 3.1.2 源码核查

## 问题

当前 MCP server 配置缺少 `protocolVersion` 时，Perihelion 固定使用 legacy `initialize` lifecycle；只有显式填写：

```json
{
  "protocolVersion": "2026-07-28"
}
```

才会使用 modern `server/discover` lifecycle。

因此，面对 modern-only MCP server，用户必须预先知道 server 的协议时代并手工标记版本，否则 Perihelion 会先发送不适用的 `initialize`，导致连接失败。这不是 legacy `initialize` 内部的普通版本选择失败，而是 2025-era 与 2026-era 两种 lifecycle 的选择错误。

期望行为是：配置缺省时自动探测并协商；显式版本仍可用于 pin 严格协议行为。

## 当前实现事实

### Perihelion 的缺省选择

`peri-middlewares/src/mcp/client/transport.rs` 当前将：

```text
protocolVersion = 2026-07-28 → ClientLifecycleMode::Discover
protocolVersion 缺失       → serve_client / legacy initialize
```

硬编码在 `lifecycle_for`、`connection_mode` 和 `serve_client_auto` 中。函数名包含 `auto`，但缺省分支并未使用 rmcp 的 `ClientLifecycleMode::Auto`。

`peri-acp-types/src/plugin.rs` 的配置注释也把未配置定义为 legacy 行为。

### rmcp 3.1.2 已有 Auto 模式

当前 lockfile 使用 `rmcp 3.1.2`。该版本提供：

```rust
ClientLifecycleMode::Auto {
    preferred_versions: Vec<ProtocolVersion>,
    legacy_version: Option<ProtocolVersion>,
}
```

其行为是先调用 `server/discover`；当响应为 JSON-RPC `METHOD_NOT_FOUND` 时，在同一 transport 上回退到 legacy `initialize`。modern lifecycle 内还会根据 `DiscoverResult.supported_versions` 或 `UnsupportedProtocolVersionError.data.supported` 选择共同版本并重试。

### rmcp Auto 的已知限制

rmcp 3.1.2 的 `Auto` 不是完整的 transport-aware 探测：

- 仅在 `METHOD_NOT_FOUND` 时回退；
- probe timeout、EOF、transport close、其他 legacy 错误不会回退；
- 在同一 transport 上执行 discover 与 initialize；
- stdio server 若收到 initialize 前未知请求后退出，无法在原 child process 上继续；
- Perihelion 当前外层 timeout 包住整个 lifecycle，沉默的 legacy server 会整体超时。

官方 TypeScript SDK 的 stdio auto 模式使用 disposable sibling process 进行 probe，再启动真实 session process，以避免 probe 消耗或终止唯一的 child process。该策略会导致 server 启动两次，可能带来启动副作用和额外延迟，不应在没有实证需求时直接引入。

## 官方协议依据

MCP `2025-11-25` 及之前使用 legacy lifecycle：

```text
initialize → initialize result → notifications/initialized
```

其中 `initialize.protocolVersion` 负责 legacy era 内部的版本选择，但不能自动升级到 2026 modern lifecycle。

MCP `2026-07-28` 及之后使用 per-request metadata，并通过 `server/discover` 或 `UnsupportedProtocolVersionError` 完成 modern 版本发现与纠正。

Dual-era client 应先探测 modern era；识别到 legacy peer 后再回退 `initialize`。参考：

- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
- https://modelcontextprotocol.io/specification/draft/basic/versioning
- https://ts.sdk.modelcontextprotocol.io/v2/migration/support-2026-07-28

## 建议语义

| 配置 | 建议行为 |
| --- | --- |
| 未配置 `protocolVersion` | 自动探测：优先 modern，必要时回退 legacy |
| `protocolVersion: "2026-07-28"` | pin modern，只使用 discover，不回退 legacy |

如未来需要强制 legacy，新增显式 negotiation mode，而不是继续把配置缺失解释为 legacy。例如：

```json
{
  "versionNegotiation": "legacy"
}
```

本 issue 不要求立即引入该新配置字段。

## 建议实施阶段

### 阶段一：使用 rmcp Auto

将未配置版本映射为：

```rust
ClientLifecycleMode::Auto {
    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
    legacy_version: None,
}
```

显式 `2026-07-28` 保持 `Discover`，作为 modern pin。

该阶段应保持 stdio 与 Streamable HTTP 共用现有连接结构，不引入双进程 probe。

### 阶段二：按兼容性证据决定 stdio probe

只有实际确认需兼容以下 server 时，再设计 disposable sibling probe：

- 对 `server/discover` 沉默；
- 收到 initialize 前请求后退出；
- 返回非 `METHOD_NOT_FOUND` 的 legacy 错误。

若实施，必须定义独立 probe timeout、child kill/reap、stderr 处理、缓存失效和重复启动副作用。

## 验收标准

- [ ] 未配置 `protocolVersion` 时，首先执行自动时代协商，而不是固定发送 legacy `initialize`。
- [ ] 未配置时可以连接支持 `server/discover` 的 2026-07-28 server。
- [ ] 未配置时，discover 返回 `METHOD_NOT_FOUND` 后可以回退并完成 legacy initialize。
- [ ] modern server 返回 `UnsupportedProtocolVersionError` 时，从双方支持版本中选择共同版本；没有交集时报清晰错误。
- [ ] 显式 `protocolVersion: "2026-07-28"` 保持 modern pin，遇到 legacy server 不静默回退。
- [ ] default handler 与 `ChannelHandler` 使用相同协商策略。
- [ ] stdio 与 Streamable HTTP 均覆盖协商测试。
- [ ] initialize 与 reconnect 使用相同策略，不发生首次连接和重连语义漂移。
- [ ] 错误信息区分：无共同版本、legacy fallback 失败、认证失败、transport 失败和 timeout。
- [ ] 配置注释、示例与代码索引不再声称“未配置等于 legacy”，并按 `DOC-UPDATE-001` 更新受影响事实源。
- [ ] 如果 rmcp Auto 对真实 stdio server 仍不足，先保留失败证据，再单独评审 sibling probe，不能把所有 transport 错误都误判为 legacy。

## 建议测试矩阵

| 配置 | Server 行为 | 期望 |
| --- | --- | --- |
| 缺省 | discover 成功，支持 2026-07-28 | modern 连接成功 |
| 缺省 | discover 返回支持版本列表 | 选择双方共同版本 |
| 缺省 | discover 返回 `METHOD_NOT_FOUND` | fallback initialize 成功 |
| 缺省 | discover 返回 recognized modern version error | 保持 modern，不回退 legacy |
| 缺省 | 无共同协议版本 | 清晰失败 |
| 显式 2026-07-28 | modern server | 连接成功 |
| 显式 2026-07-28 | legacy server | pin 失败，不回退 |
| 缺省 | probe timeout / EOF / child exit | 记录 rmcp 现有行为，为阶段二提供证据 |

## 非目标

- 本 issue 不要求立刻实现 stdio sibling process probe。
- 不把认证失败、HTTP 5xx 或一般 transport 故障当作 legacy 证据静默回退。
- 不在没有规范依据时猜测 server 版本。
- 不改变 MCP capabilities、subscriptions 或工具调用语义。

## 相关文件

- `peri-middlewares/src/mcp/client/transport.rs`
- `peri-middlewares/src/mcp/client/mod.rs`
- `peri-middlewares/src/mcp/client/pool.rs`
- `peri-acp-types/src/plugin.rs`
- `peri-middlewares/src/mcp/transport.rs`
- `Cargo.lock`
