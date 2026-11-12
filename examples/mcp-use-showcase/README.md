# mcp-use Showcase

用 [mcp-use](https://docs.mcp-use.com/typescript/getting-started/welcome) TypeScript SDK 2.12 搭的 MCP server 演示。启动后可在 Peri 中直接试 tools、resources、prompts、middleware、notifications、subscriptions、skills，以及 MRTR sampling / elicitation。

mcp-use v2 服务端只有 Web-standard `fetch`（HTTP stateless），没有 stdio transport，因此这里用常驻 HTTP server，再通过 demo 目录内 `.mcp.json` 的 `url` 接入 Peri。

## 启动

1. 安装依赖：`cd examples/mcp-use-showcase && bun install`
2. 启动 server：`bun run dev`
3. 另开终端启动 Peri（server 不会被 `.mcp.json` 自动拉起）

demo 目录内自带 `.mcp.json`，在该目录启动 Peri 即可加载。

默认地址：

- MCP：`http://127.0.0.1:31337/mcp`
- Inspector：`http://127.0.0.1:31337/mcp/inspector`
- 健康检查：`http://127.0.0.1:31337/health`

端口冲突时同时改 `package.json` 的 `dev` 脚本和 demo 目录内 `.mcp.json`。

## 在 Peri 中试用

```text
调用 mcp-use-showcase 的 echo 工具，text 传 hello
调用 get_testing_guide，按返回的步骤验收这个 MCP
列出笔记并总结 mcp 相关的内容
把 demo://state 的主题改成 dark，再读一遍这个资源
```

`.mcp.json` 内容如下：

```json
"mcp-use-showcase": {
  "type": "http",
  "url": "http://127.0.0.1:31337/mcp",
  "subscriptions": {
    "resources": ["demo://state"],
    "resourcesListChanged": true,
    "toolsListChanged": true
  }
}
```

Peri 默认支持 `subscriptions/listen`，启动后会按上述配置建立长流；调用 `set_theme` 会收到 `notifications/resources/updated`（`uri="demo://state"`），调用 `create_note` 会收到 `notifications/resources/list_changed`，资源缓存随之刷新。订阅不是 initialize 的 client capability，`client_info.capabilities` 没有订阅字段；验收以 listen 是否建立、通知是否到达为准。其他宿主只有实际调用返回 `Method not found` 时才记 SKIP。

## 功能地图

| 文档侧边栏 | 本 demo 的实现 |
| --- | --- |
| Tools | 参数/输出 schema、annotations、structuredContent、错误结果、多种 content block |
| Resources | 静态资源、模板资源、二进制资源、多 content 返回、completions |
| Prompts | 带参数与 completable 的 prompt |
| Middleware | Hono HTTP middleware、`mcp:*` 事件中间件、tools/list 过滤 |
| Elicitation | `request_approval` 通过 MRTR `input_required` 请客户端确认 |
| Sampling | `summarize_note_with_host_model` 通过 MRTR 请求宿主模型 |
| Resource Subscriptions | `set_theme` / `create_note` 触发资源更新通知 |
| Notifications | 进度、日志、自定义 `demo/task-status` 通知 |
| Skills over MCP | `skills/showcase-test-guide/`：目录存在即自动发现，无需额外开关；验收指南 + 失败排查，`get_testing_guide` 是等价工具入口 |
| OpenAPI | `src/openapi.ts` 从 OpenAPI 文档生成工具（独立 entry） |
| Proxy / Compose | `scripts/gateway.ts` 把上游 MCP server 代理到另一个端点 |
| MCP Client | `scripts/client.ts`、`scripts/subscribe.ts` |
| MCP Agent | 未包含：需要 LLM provider 与 LangChain peer 依赖，与 server 能力无关 |
| Tooling | `mcp-use dev / typecheck / client / build / start`，Inspector 位于 `/mcp/inspector` |
| Views / Apps | 未包含：需要 React 渲染宿主，Peri TUI 不消费 |
| OAuth / Deployment | 未包含：需要外部 provider 与部署环境 |

Skills 遵循官方零配置约定：项目根存在 `skills/` 即自动发现，不需要 `skills: true`。参考 [mcp-use Skills 文档](https://docs.mcp-use.com/typescript/server/skills)。

## 工具

| 工具 | 演示点 |
| --- | --- |
| `echo` | 基础参数校验与 annotations |
| `search_notes` | outputSchema + structuredContent |
| `create_note` | 写入状态、`resources/list_changed` + `resources/updated` |
| `set_theme` | 资源更新通知 |
| `reset_demo_state` | `destructiveHint`，触发 Peri HITL 确认 |
| `slow_task` | progress、日志、自定义通知 |
| `request_approval` | MRTR elicitation |
| `summarize_note_with_host_model` | MRTR sampling |
| `client_info` | 客户端信息、能力协商、请求头、Views 支持 |
| `fail_demo` | `isError` 结果 |
| `get_testing_guide` | 验收步骤；Skills 自动发现不可用时的等价入口 |

`_internal_diagnostics` 会被 `mcp:tools/list` 中间件过滤，不会出现在工具列表。

## 资源

| URI | 内容 |
| --- | --- |
| `demo://readme` | 演示说明 |
| `demo://state` | 可变状态（theme / revision / noteCount） |
| `demo://notes` | 笔记索引 |
| `demo://notes/{id}` | 模板资源，带 completion |
| `demo://report` | 多 content 返回（markdown + JSON） |
| `demo://banner` | base64 PNG 二进制资源 |
| `skill://showcase-test-guide/SKILL.md` | 验收指南；`references/failure-triage.md` 为失败排查 |

## 脚本

先保持 `bun run dev` 运行，再执行：

```bash
bun run client      # mcp-use client：tools/resources/templates/prompts/progress/MRTR
bun run subscribe   # 2026 subscriptions/listen：资源更新、列表变化、日志通知
bun run openapi     # 端口 31338，OpenAPI 生成工具 + 本地 mock REST API
bun run gateway     # 端口 31341，代理 31340 的上游 MCP server
bun run typecheck
```

`client` 与 `subscribe` 的 server 地址可用 `SHOWCASE_URL` 覆盖。

OpenAPI 示例把 POST body 放在 `body` 参数下：

```bash
bunx mcp-use client openapi tools call rest_create_note \
  '{"body":{"id":"x","title":"demo","tags":["demo"]}}'
```

## 已知限制

1. 状态保存在进程内，重启后回到种子数据；这是单进程演示，不是持久化设计。
2. Skills 自动发现需要 Peri ≥ 0.2.x（rmcp 3.9.0）。早期 rmcp 会把 mcp-use 注入的 `_meta["io.modelcontextprotocol/serverInfo"]` 误判成 `CallToolResult`，导致 `skills/list` 报“非预期响应类型”；升级后该问题不再出现。`get_testing_guide` 与 `skill://showcase-test-guide/SKILL.md` 仍可直接调用，用于查看验收指南。
3. MRTR sampling / elicitation 依赖客户端能力；Peri 对这两项能力未声明时，对应工具会返回明确错误，`client.ts` 中已演示支持方。
4. 未包含 MCP Apps / Views：需要 React 渲染宿主，Peri TUI 不消费。
5. 未包含 OAuth 与部署：需要外部 provider / 云端环境，不属于本地 demo 范围。

## 目录

```text
.mcp.json                  demo 目录自带的 MCP 接入配置
index.ts                   主 server（tools/resources/prompts/middleware/skills）
src/openapi.ts             OpenAPI -> MCP 独立 entry（同进程 mock REST API）
scripts/client.ts          mcp-use client 全能力示例
scripts/subscribe.ts       2026 subscriptions/listen 示例
scripts/gateway.ts         proxy 网关示例
skills/showcase-test-guide/ Skills over MCP：验收指南 + 失败排查
```
