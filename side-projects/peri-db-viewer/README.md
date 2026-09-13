# Peri DB Viewer

本地只读会话查看器。

这是 Peri `threads.db` 的本地诊断页面。Hono API 与旧 UI 共用一个只读 `ViewerDataAdapter`；消息解析和 schema 能力探测委托给 `agent-defect-analyzer/src/data/loader.ts`，viewer 不复制消息 normalization。

Dashboard 与工具统计覆盖全库自有消息，不重复计入继承快照；详情页可查看继承内容。
离线 `report` 默认仅分析可见主会话，并排除有冲突的调用配对，因此两者的总数不应直接比较。
研究结论以带范围、解析质量与证据引用的离线报告为入口，viewer 用于阅读上下文。

## 运行

```bash
bun install
PERI_DB_PATH=~/.peri/threads/threads.db bun run dev
```

服务固定绑定 `127.0.0.1`，端口默认为 `8741`，可用 `PERI_DB_VIEWER_PORT` 覆盖。`PERI_DB_PATH`（或兼容的 `PERI_THREADS_DB`）指定 SQLite 文件。数据库以只读方式打开；缺库、坏 schema 会在启动时失败并返回明确错误，停止服务会关闭连接。

页面使用仓库内固定路径 `/assets/echarts.min.js` 的 vendored ECharts 6.1.0 资源，不依赖 CDN。资源许可与第三方声明见 [ECharts LICENSE](src/public/assets/ECHARTS-LICENSE.txt) 和 [ECharts NOTICE](src/public/assets/ECHARTS-NOTICE.txt)。

## 验证

```bash
bun test src/app.test.ts
bun run typecheck
```

测试覆盖 Hono `app.request` API fixture、真实 SQLite fixture、共享 normalization 的 top-level `tool_calls`、legacy 双写去重、错误结果配对、缺库/坏 schema、错误分页参数和关闭连接。

线程详情消息接口支持 `page`/`perPage`（默认每页 100，单页上限 500），返回 `total`、`offset` 和 `hasMore`；详情页的 “Load more messages” 会继续请求后续页，避免长会话一次性加载全部历史。

`export-analysis.ts` 与 `user-prompts.ts` 是历史一次性查询脚本，已移除；证据导出应接入 agent-defect-analyzer 的统一 CLI，而不再恢复独立入口。
