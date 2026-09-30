
# OTLP v4 导出

实现、参数和测试入口见 `../docs/code-index/langfuse-client.md`。

传输走 `/api/public/otel/v1/traces`，使用项目公钥/私钥 Basic auth 与
`x-langfuse-ingestion-version: 4`。观测在结束后一次导出完整 Create，不能通过重发
同 ID 的 span 更新；Update/Score 在此入口显式拒绝，Session/SDK record 不产生 span。

独立配置队列容量、批大小、并发与字节预算。`Batcher::stats` 提供不含 payload 的
累计计数；flush 只确认其 FIFO 前缀，shutdown 报告部署累计发送失败并保留 join 所有权。

HTTP 200 必须解析 OTLP 响应；部分拒收返回含拒收数的 `PartialSuccess`，
不能当作完整成功，也不能整批重试。batcher 累计部分拒收批次及 span 数，
flush 失败确认和 shutdown 部署累计失败报告保留既有契约。
429/502/503/504 和可恢复传输错误使用有界重试；Retry-After 和累计期限同时生效。

这些预算保护导出边界，不是精确 RSS 上限，也不能证明远端已经落库或满足生产负载 SLO。
