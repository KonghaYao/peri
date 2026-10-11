# Parallel Search MCP

通过 Peri 的项目级 MCP 配置使用免费的网页搜索和页面内容提取，无需 Parallel 账号或 API key。本例通过固定版本的 [mcp-remote](https://github.com/punkpeye/mcp-remote) 将远端 Streamable HTTP 服务接入 Peri 的 stdio MCP 路径。匿名搜索使用 Fast 模式，适合探索和轻量使用，受服务端速率限制。详情见 [Parallel Search MCP](https://docs.parallel.ai/integrations/mcp/search-mcp)。

## 使用

1. 安装 [Node.js](https://nodejs.org/)（Node.js 22 或更新版本，需提供 `node` 和 `npx`），再按仓库 [安装说明](../../README.md#get-started) 安装 Peri，并完成模型配置。
2. 将本目录的 [`.mcp.json`](.mcp.json) 中 `parallel-search` 条目合并到你的项目根目录 `.mcp.json` 的 `mcpServers` 中，保留已有条目；也可以直接在本目录启动 Peri 体验。
3. 在包含 `.mcp.json` 的目录启动新的 Peri 会话：

   ```bash
   peri -p "使用 parallel-search MCP 搜索 Rust 官方 async 文档，给出来源链接；再用该 MCP 读取 https://doc.rust-lang.org/book/ch17-01-futures-and-syntax.html 并总结 Future。"
   ```

模型仍需使用你已配置的推理服务。`web_search` 与 `web_fetch` 已在 `system_mcp_tools` 中声明为必需工具，以原始工具名直接进入模型工具面，不经工具搜索发现：`web_search` 返回搜索结果与摘录，`web_fetch` 提取指定 URL 的内容。

`system_mcp: true` 让首轮推理等待 MCP 连接与工具列表就绪（本例启动依赖等待上限为 60 秒）；`system_mcp_tools` 中的工具随连接就绪直接进入模型工具面。服务不可用时，此示例会阻止会话启动；移除该条目可恢复不依赖它的会话。

配置通过 `npx --prefer-offline -y mcp-remote@0.14.3` 启动固定版本的桥接依赖：缓存命中时跳过每次启动的 registry 校验；首次运行仍会自动下载到 npx 缓存（首轮启动等待包含该下载耗时）。`--transport http-only` 限定远端使用 Streamable HTTP；`--enable-proxy` 允许使用已有的 HTTP(S) 代理环境配置。当前 Peri 的直接 HTTP discovery 使用 `2026-07-28`，而 Parallel 接受的版本截至 `2025-11-25`，因此本例使用 stdio 桥接。

这是可选的 stdio 配置。本示例目录的 [`.peri/settings.json`](.peri/settings.json)（`config.meta_harness.WebMiddleware: false`）关闭内置 `web` 实例，使搜索与页面提取完全由 parallel-search 提供；复制到自己的项目时可移除该文件以保留内置 `WebSearch` / `WebFetch`。配置中的 `User-Agent` 标识此 Peri 示例。不要为匿名使用添加 `Authorization` 或 OAuth 配置。

若已有同名服务，请先合并或改名；工具名会随服务名变化。修改配置后重新启动会话。遇到速率限制时，按服务响应等待后重试。
