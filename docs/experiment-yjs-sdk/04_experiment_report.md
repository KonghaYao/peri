# 实验 4：12k 工具历史后的 16 MiB 流式复制与视图

状态：**DONE_WITH_CONCERNS**。2026-10-05（Asia/Shanghai）完成计划内组合实验：最终候选 **3/3 passed**，基线 **3/3 因 RSS 超过 2 GiB 截断**。基线没有完成全文，不能计算全程加速比，也没有把截断当作全量通过。

## 为什么追加与怎样测量

原工具/view 场景只在历史后追加 128 KiB；原 16 MiB stream 从空历史开始，未覆盖用户关心的组合。此次使用同一生成器先投影 12,000 工具，再建立真实 demo `SessionDocStream`、冷副本和接收端 SessionViewStore，追加 16,384 × 1 KiB 文本。

双方都实际执行 demo base64 编码、JSON body 字节统计、base64 解码及 Yjs apply；每 **64 分片** flush 并 `await Promise.resolve()`，保持接收端视图发布频率相同。候选的同步器仍为 16 ms / 64 KiB / 256 updates / 4 MiB subscriber 配置，最后 completed → flush → close → 视图微任务都纳入 stream `endToEndMs`。

stream 计时排除历史 seed、初始冷快照和计时区外全文验证。初始和最终公开视图均核对全部工具数量、顺序、归属、状态、输入和结果；引用经 owner 的 `readPayload` 精确读取。预期 256 次流式发布与 1 次完成发布；最后检查 16 MiB 文本 SHA-256 和接收端 completed。初始快照的 JSON 字节采用等价长度计算，避免仅为计数额外分配一份巨大 JSON 字符串。双方不模拟 HTTP/SSE 网络、JSON.parse、DOM 或绘制。

源根分别为第一轮冻结基线和第三轮最终冻结候选，运行时/依赖/生成器保持不变。脚本：[combined.ts](../../npm-packages/@peri-sdk/benchmarks/combined.ts)。所有压力子进程串行；场景整体上限仍为 **120 秒 / 2 GiB**，包括 seed、冷复制、stream 与验证，不只约束计时区域。

```sh
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-baseline-567640f1837771442628970a56ce68c5a8f8e256 \
  --output docs/experiment-yjs-sdk/raw/baseline-combined \
  --api legacy --scenarios combined --samples 3
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-final-round3 \
  --output docs/experiment-yjs-sdk/raw/final-combined \
  --api candidate --scenarios combined --samples 3
```

原始数据：[baseline-combined](raw/baseline-combined/summary.json)、[final-combined](raw/final-combined/summary.json)。[扩展 harness 归档](raw/final-combined/harness.tar.gz)、[候选 provenance](raw/final-combined/provenance.json)、[基线 provenance](raw/baseline-combined/provenance.json) 保存脚本和来源；候选源码复用第三轮归档。复现应使用新的输出目录，原数据不覆盖。

## 调度异常与排除

第一组 baseline-combined 启动后获知主代理已按之前的空闲通知开始 SDK build/test。为避免混入并发 CPU 影响，整组原样移至 [baseline-combined-overlap](raw/baseline-combined-overlap/summary.json)，并加入 [明确无效标记](raw/baseline-combined-overlap/INVALID_FOR_COMPARISON.json)。该组不参与下面任何性能结论。等待主代理确认测试终止后重新开始上述正式基线，随后串行运行最终候选；正式组没有与其他构建或测试重叠。

## 基线：历史冷复制成功，长流式中触发预算

三个基线样本先完成 12,000 工具的初始公开视图全文校验。冷快照均为 **115,968,355 V1 bytes / 154,624,565 JSON body bytes**；此时 turn 尚未完成，所以快照与单独 tools 场景略有不同。

| 样本 | 截断前分片数 / 16384 | 已投递文本 MiB | 已发布视图 | 增量帧 | 增量 V1 bytes | 增量 JSON bytes | 峰值 RSS MiB | 场景 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2752 | 2.6875 | 43 | 2753 | 2968078 | 4217542 | 2053.63 | 2700.21 |
| 2 | 2816 | 2.7500 | 44 | 2817 | 3037262 | 4315974 | 2067.81 | 2666.34 |
| 3 | 2816 | 2.7500 | 44 | 2817 | 3037262 | 4315974 | 2064.83 | 2659.02 |

三个子进程均 exit 1、`status=truncated`、错误 `RSS_BUDGET_EXCEEDED`，发生于 `combined_stream` 的预算检查，未到 completed 和最终全文验证。43/44/44 次发布中，首个未改变工具对象复用都是 0。记录的约 465–473 ms stream 时间只是已完成前缀的耗时，**不是 16 MiB 的总耗时**。RSS 的检测间隔为每 64 分片与父进程每 250 ms 轮询，超过阈值后立即终止该场景，因此实际峰值略高于 2048 MiB。

## 最终候选：预算内完成整个组合

候选三个样本的初始热快照均为 **23,059,316 V2 bytes / 30,745,880 JSON body bytes**。流式增量均为 **16,796,906 V2 bytes / 22,443,213 JSON body bytes / 513 帧**。初始快照和增量是不同计数，不能混为单一帧成本。

| 样本 | seed 投影 ms | 初始快照总 ms | stream 端到端 ms | stream apply ms | 视图发布 / 身份复用 | 场景总 ms | 峰值 RSS MiB |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| 1 | 141.382 | 164.052 | 327.620 | 10.175 | 257 / 257 | 939.622 | 1849.50 |
| 2 | 148.743 | 157.394 | 316.346 | 10.809 | 257 / 257 | 912.106 | 1851.19 |
| 3 | 149.772 | 168.857 | 339.749 | 10.325 | 257 / 257 | 945.164 | 1763.81 |

候选 stream 端到端均值 **327.905 ms**、中位数 **327.620 ms**、样本标准差 **11.704 ms**；apply 均值 **10.436 ms**、中位数 **10.325 ms**、标准差 **0.332 ms**。全场景峰值约 **1.72–1.81 GiB**，包括源文档、完整载荷、base64 缓冲、冷副本、视图与前后两轮全文读取。

每个候选最终都保留 **12,000 个工具 / 1,200 个历史 assistant**，精确验证参数与结果。初始和最终验证各读取 12,000 个外置结果，共各 **98,964,890 bytes**；它们是计时区外的进程内按需读取，没有假设网络已下载这些内容。stream 新消息精确为 **16,777,216 bytes**，接收端 completed，SHA-256 为 `732dd2b3f284d2273326b8acf57f014a895f513bfc79770eb963226aeaef8cb6`。最后完成 flush 发送 1 帧。

## 观察、推断与限制

**观察**：在同样的 2 GiB 进程预算和每 64 分片发布一次视图的负载下，基线在约 2.7 MiB 文本处截断，候选完整完成 16 MiB，并保留全部工具全文与终态。候选增量二进制载荷约为文本本身的 1.0012 倍；这个组合没有观察到整份历史随每批重新发送。

**推断**：大结果隔离与未变视图对象复用使此本地组合在设定预算内可完成，符合目标方向。但 RSS 是运行时与临时分配的峰值，未做 heap profile，不能把两组峰值差异唯一归因于某一种数据结构。候选接近资源上限，结果也不能推广为所有 2 GiB 容器、多个客户端或更大输入都能完成。

基线没有完整 16 MiB 结果，因此不报告组合的全程加速比或全程通信字节减少比例；初始快照的对比是完整、可比较的数据。该实验验证本地复制和接收端数据视图，不涉及真实网络延迟、浏览器渲染或无限长会话，也不代替断线恢复、慢消费者和取消等生命周期契约测试。
