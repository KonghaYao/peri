# Memory MCP 0.1.0 验证记录

日期：2026-11-16。Peri 基线：`79e7d3ebc95eee30be153ea6b22ebb8ee01ae478`。

环境：Linux x64；Node 24.22.1；npm / npx 11.9.0；Bun / bunx 1.4.4。依赖锁定在 package-lock.json；mcp-use 2.12.4、MCP client/server SDK 2.3.1。没有使用用户现有 Hindsight 的地址、凭据或数据。

## 已通过

| 检查 | 结果 |
| --- | --- |
| TypeScript `npm run check` | 通过 |
| esbuild 单入口 ESM 构建 | 通过；dist/index.js 带可执行 shebang |
| `npm test` | 7 / 7 通过，无跳过 |
| 真实 stdio 子进程 | 官方 0728 client 完成发现、工具、资源、订阅 ID、取消、重新订阅与 EOF |
| 独立 importer 子进程 | JSON 文件导入完成后使 MCP 快照 stale；两者没有进程调用或 MCP 导入工具 |
| mcp-use HTTP server | bearer、Origin、Inspector 路径限制；工具调用、资源订阅；未知订阅 URI 拒绝 |
| Hindsight 契约替身 | 实际 HTTP 请求的路径、body、鉴权头、同步 retain 响应；稳定文档 ID、部分失败、超时、错误脱敏 |
| 并发与生命周期 | 后发 refresh 胜出；导入使快照失效；关闭连接、取消订阅与 stdin EOF 不留进程 |
| npx 远程 tarball | 从临时 HTTP URL 安装并运行 bin；停止下载服务器后 `--offline` 成功 |
| bunx 远程 tarball | `--bun --package @peri/memory-plugin@URL` 成功；停止下载服务器后 `--no-install` 成功 |
| 缓存后的真实 stdio | npx 与 bunx 均通过 0728 发现、4 个工具和 recall→HTTP 契约替身 |
| 缓存后的独立 CLI | npx 与 bunx 均从 stdin 执行 import dry-run，零后端写入 |

完整测试名和输出见交付 ZIP 的 `validation.log`。分发验证程序在源码 `scripts/distribution-smoke.mjs`；它自行启动临时 tarball HTTP 服务与本地后端替身，并在缓存复用检查前停止 tarball 服务。

```bash
npm ci
MCP_USE_ANONYMIZED_TELEMETRY=false npm run check
MCP_USE_ANONYMIZED_TELEMETRY=false npm test
npm pack
BUN_BIN=/absolute/path/to/bun node scripts/distribution-smoke.mjs
```

包内已在 mcp-use 模块首次加载前关闭其官方遥测开关。测试过程中发现的默认遥测外连已被自动审批拦截，修正后验证成功，未绕过拦截。

## 验证边界与维护员操作

1. 本地 HTTP 契约替身不是 Hindsight 模型服务。尚未验证用户部署版本、生产认证、模型抽取质量、实际检索效果或网络延迟。
2. npm tarball 已构建，但没有发布到 npm 或固定 HTTPS 地址；文档中的下载域名是示例。缓存验证使用本机 HTTP 源，未验证生产 CDN。
3. Bun 已验证包安装、缓存、stdio recall 和 import dry-run；完整 HTTP 服务端测试在 Node 运行。macOS / Windows 未在本次环境执行。
4. 本地源码变更只新增 `npm-packages/memory-plugin`。没有修改远端 main、Peri middleware、Rust 发布流程或数据库。
5. Peri 通用资源订阅目前提醒更新，不自动注入记忆正文。真实宿主联调应验证发现→订阅确认→回读→刷新→更新提醒→回读这一条路径。
6. 维护员应使用单独验收 bank：导入一条测试消息和一条 compact，确认同步 retain 后 recall 可找到来源，再启动 Peri stdio 配置验证上下文更新。通过后使用部署方确认的正式 bank。不要把验收消息写入用户正式记忆。
7. 应用补丁后在最新 main 重跑检查与测试，提交 PR；是否发布 tarball、启用生产配置和合并由仓库正常流程决定。

补丁应用检查、发布包文件清单和 SHA-256 在交付 ZIP 中附带。设计与限制见 [DESIGN.md](DESIGN.md)。
