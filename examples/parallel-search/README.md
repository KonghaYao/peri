# Parallel Search MCP

通过 Peri 的项目级 MCP 配置使用免费的网页搜索和页面内容提取，无需 Parallel 账号或 API key。本例通过固定版本的 [mcp-remote](https://github.com/punkpeye/mcp-remote) 将远端 Streamable HTTP 服务接入 Peri 的 stdio MCP 路径。匿名搜索使用 Fast 模式，适合探索和轻量使用，受服务端速率限制。详情见 [Parallel Search MCP](https://docs.parallel.ai/integrations/mcp/search-mcp)。

## 使用

1. 安装 [Node.js](https://nodejs.org/)（Node.js 22 或更新版本，需提供 `node` 和 `npm`），再按仓库 [安装说明](../../README.md#get-started) 安装 Peri，并完成模型配置。
2. 在本目录运行 `npm ci --ignore-scripts` 安装固定版本的桥接依赖。若在自己的项目使用，先在项目根目录运行 `npm install --save-exact --ignore-scripts mcp-remote@0.14.3`。然后将本目录的 [`.mcp.json`](.mcp.json) 中 `parallel-search` 条目合并到你的项目根目录 `.mcp.json` 的 `mcpServers` 中，保留已有条目。也可以直接在本目录启动 Peri 体验。
3. 在包含 `.mcp.json` 的目录启动新的 Peri 会话：

   ```bash
   peri -p "使用 parallel-search MCP 搜索 Rust 官方 async 文档，给出来源链接；再用该 MCP 读取 https://doc.rust-lang.org/book/ch17-01-futures-and-syntax.html 并总结 Future。"
   ```

模型仍需使用你已配置的推理服务。MCP 工具通过 Peri 的工具发现与执行路径使用：`SearchExtraTools` 发现 `parallel-search` 工具，`ExecuteExtraTool` 调用发现结果中的工具名。`web_search` 返回搜索结果与摘录，`web_fetch` 提取指定 URL 的内容。

`system_mcp: true` 让首轮推理等待 MCP 连接与工具发现（本例启动依赖等待上限为 60 秒）；空的 `system_mcp_tools` 保持工具通过发现路径使用。服务不可用时，此示例会阻止会话启动；移除该条目可恢复不依赖它的会话。

配置通过 `node ./node_modules/mcp-remote/dist/proxy.js` 启动已安装的桥接依赖，无需在 MCP 启动期间下载 npm 包。`--transport http-only` 限定远端使用 Streamable HTTP；`--enable-proxy` 允许使用已有的 HTTP(S) 代理环境配置。当前 Peri 的直接 HTTP discovery 使用 `2026-07-28`，而 Parallel 接受的版本截至 `2025-11-25`，因此本例使用 stdio 桥接。

这是可选的 stdio 配置，不替换内置 `WebSearch` / `WebFetch`，也不更改模型或其他 MCP 设置。配置中的 `User-Agent` 标识此 Peri 示例。不要为匿名使用添加 `Authorization` 或 OAuth 配置。

若已有同名服务，请先合并或改名；工具名会随服务名变化。修改配置后重新启动会话。遇到速率限制时，按服务响应等待后重试。
