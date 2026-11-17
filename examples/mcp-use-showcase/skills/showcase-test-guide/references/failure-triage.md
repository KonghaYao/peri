# 失败排查

按以下顺序定位，先区分“宿主不支持”和“server 有问题”。

## 1. 工具未出现在 tools/list

1. 确认 Peri 从 `examples/mcp-use-showcase/` 启动，且该目录内 `.mcp.json` 的 URL 是 `http://127.0.0.1:31337/mcp`。
2. `curl http://127.0.0.1:31337/health` 应返回 `ok`。
3. `_internal_diagnostics` 是唯一被中间件隐藏的工具；其他工具缺失才视为失败。
4. 宿主可能给工具加命名空间前缀，按名称后缀匹配。

## 2. skills/list 告警或技能未自动发现

之前的宿主问题已修复：Peri ≥ 0.2.x（rmcp 3.9.0）不会再因 `_meta`
把 `skills/list` 判为非预期响应。若仍在旧 Peri 上看到该告警：

- 这是宿主客户端问题，不是 server 的 skills 数据错误。
- 本 server 是零配置发现：只要项目根有 `skills/`，`mcp-use dev/build` 就会暴露。
- 旧宿主上的绕行方式：
  1. 调用 `get_testing_guide` 获取验收步骤。
  2. 直接用资源读取 `skill://showcase-test-guide/SKILL.md`。
  3. `demo://readme` 中也列出了技能 URI。
- 不要在测试报告中把该告警记为 server FAIL。

## 3. 资源读取失败

| 现象 | 排查 |
| --- | --- |
| `demo://notes/xxx` 报不存在 | 先读 `demo://notes` 确认 id；只允许模板中的 id |
| `demo://state` 内容旧 | 重新读取；确认上一次 `set_theme` 的 revision 已递增 |
| `demo://banner` 无文本 | 预期是 `blob`（image/png），不是 text |
| 宿主的资源工具要求完整 URI | 使用 `demo://` 前缀，不要省略 |

## 4. 收不到通知

1. 先检查 `.mcp.json`：`subscriptions.resources` 是否包含 `demo://state`，`resourcesListChanged` / `toolsListChanged` 是否为 `true`。
2. 确认 `subscriptions/listen` 已建立并收到 acknowledged filter（`notifications/subscriptions/acknowledged`）。Peri 使用 MCP 2026-07-28，走 listen。
3. 不要先看 `client_info.capabilities`：订阅不是 initialize 的 client capability，那里没有订阅字段；缺字段不代表宿主不支持订阅。
4. 订阅建立后调用 `set_theme`（`{"theme":"dark"}`），应收到 `notifications/resources/updated`，`params.uri="demo://state"`；调用 `create_note` 后 server 会发 `notifications/resources/list_changed`（该通知不带 `uri`，仅含 `_meta`）。Peri 收到后只失效资源缓存，不注入会话提醒，属正常。
5. listen 返回 `Method not found` 时，说明宿主或端点未实现 2026 listen。mcp-use 的 legacy stateless 模式只实现 modern listen，legacy `resources/subscribe` 会返回 `Method not found`，不能据此判 server 失败。
6. 连接关闭或会话结束时收到“订阅流异常中断”属于正常收尾；不要在会话结束后重启订阅。
7. `create_note` 后会话里没有 `list_changed` 提醒：用 `bun run subscribe` 或 SDK 客户端确认 server 发送，或直接重读 `demo://notes` 确认新笔记存在；不要判 FAIL。
8. `slow_task` 没有 `demo/task-status` / `notifications/message` 提醒：Peri 当前不把这些通知注入会话，server 实际会发送；用独立客户端确认。`notifications/message` 在 MCP 2026-07-28 已废弃（SEP-2577），仅在请求 `_meta` 含 logLevel 时才是协议要求，不能作为 server FAIL 依据。

## 5. MRTR 返回“未声明能力”

`request_approval` 依赖 elicitation，`summarize_note_with_host_model` 依赖 sampling。
宿主未声明对应能力时，server 返回 `isError=true` 是设计行为，记 SKIP。Peri rmcp 3.9.0 当前 `sampling=false`、`elicitation=false`，两项 SKIP 是正常基线。

## 6. 参数校验失败

- `create_note.id` 只允许小写字母、数字和连字符。
- `set_theme.theme` 只能是 `light` 或 `dark`。
- `reset_demo_state.confirm` 必须是字面量 `true`。
- `echo.text` 不能为空字符串。
- 参数校验错误不应导致 server 崩溃；随后再调用 `echo` 验证。

## 7. 状态与进程

状态保存在模块作用域，随 dev server 进程存活：

- 重启 server 会恢复种子数据、`theme=light`，`revision` 从 1 重新开始。
- `reset_demo_state` 不会把 revision 归零，而是继续递增。
- 多个宿主共享同一 server 进程时，会看到彼此的修改；这是 demo 预期，不是隔离缺陷。

## 8. 其他端点

- 主 MCP：`http://127.0.0.1:31337/mcp`
- Inspector：`http://127.0.0.1:31337/mcp/inspector`
- OpenAPI demo：`bun run openapi`，端点 `http://127.0.0.1:31338/mcp`
- Proxy demo：`bun run gateway`，端点 `http://127.0.0.1:31341/mcp`

OpenAPI 与 proxy 是独立入口，不在主 server 的工具列表里。

## 9. Prompts 无宿主入口

Peri 当前不消费 MCP prompts。用独立 SDK 客户端执行 `listPrompts` / `getPrompt` 验证 server；宿主侧记录“未消费”，不判 FAIL。可用 `scripts/client.ts` 或临时 `bun -e` 脚本。

## 10. 宿主展示能力基线（Peri rmcp 3.9.0，2026-11-12 实测）

| 能力 | 实测 | 判定 |
| --- | --- | --- |
| `resources/updated` | 以会话提醒到达 | PASS |
| `resources/list_changed` | server 发送；宿主仅失效缓存 | PASS（宿主不提醒） |
| `demo/task-status` | server 发送；宿主不注入 | SKIP（宿主未展示） |
| `notifications/message` | 2026-07-28 已废弃；宿主不注入 | SKIP |
| MRTR sampling/elicitation | 均未声明 | SKIP |
| prompts | server 有 2 个；宿主无入口 | 用 SDK 验证 |
