# 实验 2：热文档隔离、增量视图与 V2 批处理

状态：**DONE_WITH_CONCERNS**。2026-10-05（Asia/Shanghai）完成。本轮候选 12/12 样本 passed；补充基线 12/12 passed；小规模公共断言验证 1/1 passed。所有正式场景均为独立进程、exit 0，未超过 120 秒或 2 GiB 预算。工具投影、热快照和视图更新改善；**纯流式完整耗时增加，等待下一轮改进**。本报告对应冻结候选，不代表后续工作区修复。

## 假设、来源与方法修订

本轮验证：直接工具归属索引能否消除随历史增长的扫描；缓存是否保留全部历史并仅刷新变化对象；大结果引用是否缩小默认热同步；逐事件 V2 编码加批处理能否降低通信帧和字节。

候选在 `2026-10-04T16:31:59.062711Z` 冻结到 `/tmp/peri-yjs-sdk-candidate-round2`。Bun 1.4.2、Yjs 13.6.33、Apple M5 Pro / Darwin arm64 与第一轮一致。候选 src SHA-256：`3f5cbc18561d8c6a5997683b49aae5c182295c8bd0217918f507289bd847731e`；锁文件 SHA-256：`acf7c28963e8fbb65317a543683d400cec43ece64de3603777f6ef1510e0e367`。12 个候选进程源指纹一致。

按 [第一轮验证意见](01_verification.md) 修订：

- 公开视图逐个检查 12,000 工具的数量、顺序、assistant message 归属、turn、名称、状态、输入与结果；首个工具必须非空。每个历史 assistant 的原文本及 user 文本均保留。
- 冷接收端从公开视图取得引用，经 `SessionDocs.readPayload(ref.id)` 读取并解析精确 JSON，核对内容与声明字节数；不以 preview 代替全文。小结果仍按 inline 字段校验。
- 会话 completed 成为断言。view 的 128 次计时更新后，增加一次真实工具名称更新，验证对象失效与先前快照不变；这次附加更新不计入原 128 次性能数据。
- stream 新增 `endToEndMs`，从 user 通知之前到完成通知、显式 flush、等待 close、复制应用全部结束；包含循环内生成文本，排除初始冷快照和最终全文校验。基线补采相同区间，避免比较延后工作之前的单次 accept 耗时。
- 工具与视图也补跑相同增强 harness 的基线，控制公开历史/按需载荷校验对内存及预热的影响。第一轮原始文件完全保留，下面主要比较这些补充基线。

生成器 [workloads.ts](../../npm-packages/@peri-sdk/benchmarks/workloads.ts) **未改动**，SHA-256：`5fe3c36d10ea9b148deb605559b4bb5724d78bfd9fb672ef134291cd01856a66`。仍为 12k 工具、每 10 个切换消息、1 KiB 输入/8 KiB 结果、128 次异步更新、16,384 × 1 KiB 流、独立 1 MiB 大结果。候选大载荷阈值 4096 JSON bytes，preview 为前 512 字符。

## 复现与原始数据

在仓库根目录使用同一个 [run.ts](../../npm-packages/@peri-sdk/benchmarks/run.ts)，每个命令的所有进程串行运行：

```sh
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-baseline-567640f1837771442628970a56ce68c5a8f8e256 \
  --output docs/experiment-yjs-sdk/raw/baseline-stream-supplement \
  --api legacy --scenarios stream --samples 3
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-candidate-round2 \
  --output docs/experiment-yjs-sdk/raw/candidate \
  --api candidate --scenarios tools,view,stream,large --samples 3
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-baseline-567640f1837771442628970a56ce68c5a8f8e256 \
  --output docs/experiment-yjs-sdk/raw/baseline-enhanced \
  --api legacy --scenarios tools,view,large --samples 3
```

这些输出目录已有原始文件，CLI 会拒绝覆盖；复现应换用新目录。基线重建与第一轮 harness 归档见 [benchmark README](../../npm-packages/@peri-sdk/benchmarks/README.md)。候选的 [冻结源码归档](raw/candidate/frozen-source.tar.gz)、[本轮 harness 归档](raw/candidate/harness.tar.gz) 和 [来源/文件指纹](raw/candidate/provenance.json) 可重建本轮：将源码解压到空目录、harness 解压到该目录 `benchmarks/`，在该目录用 Bun 1.4.2 执行 `bun install --frozen-lockfile` 后运行 `benchmarks/run.ts --root . --api candidate --output <新目录>`。

完整结果：[候选 summary](raw/candidate/summary.json)、[增强基线 summary](raw/baseline-enhanced/summary.json)、[补充流式基线](raw/baseline-stream-supplement/summary.json)、[基线小合同验证](raw/baseline-contracts/summary.json)。各样本 JSON 还包含 128 个原始视图延迟、各 1,000 工具的投影耗时、阶段 RSS、退出码与内容校验。

## 工具、视图与载荷结果

下表均为 3 个独立进程的中位数；基线使用增强 harness：

| 指标 | 基线 | 候选 | 观察 |
| --- | ---: | ---: | --- |
| 12k 工具投影 ms | 1740.88 | 142.27 | 约 12.24 倍加速 |
| 双文档 V1 热快照 bytes | 115984977 | 24719251 | 减少 78.69% |
| 候选 V2 热快照 bytes | — | 23059321 | 比候选 V1 再减少 6.72% |
| 主编码冷快照 encode ms | 175.21（V1） | 70.70（V2） | 包含 schema/载荷与 codec 的共同变化 |
| 主编码冷复制 apply ms | 118.80 | 75.02 | 12k 工具仍完整可读 |
| 128 次视图总耗时 ms | 2883.21 | 52.19 | 约 55.24 倍加速 |
| 视图单次延迟中位数 ms | 22.101 | 0.337 | 约 65.54 倍加速 |
| 视图单次经验 p95 ms | 24.734 | 0.762 | 本机连续工作负载的分位数 |
| 未改变首工具对象复用 | 0/128 | 128/128 | 三个样本一致 |
| tools 全场景峰值 RSS MiB | 1456.53 | 1281.88 | 减少约 12.0% |
| view 全场景峰值 RSS MiB | 971.70 | 1072.17 | **增加约 10.3%** |
| 1 MiB 结果默认冷快照 bytes | 1051095 | 2779 | 全文另行保存和读取 |

候选各样本主要数值：

| 样本 | 工具投影 ms | 热 V2 bytes | tools RSS MiB | view 总 ms | view 中位 ms | view p95 ms | view RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 147.211 | 23059321 | 1281.88 | 52.194 | 0.337 | 0.762 | 1072.17 |
| 2 | 141.589 | 23059321 | 1275.97 | 59.204 | 0.388 | 0.763 | 1075.00 |
| 3 | 142.269 | 23059321 | 1285.31 | 48.693 | 0.336 | 0.514 | 1071.22 |

原始输入与结果 JSON 仍为 **111,050,670 bytes**。候选 12,000 个结果引用全部成功读取，共 **98,964,890 bytes**；每份冷复制工具视图验证 12,000 个工具和 1,200 个历史 assistant。工具真实更新使对应对象刷新，旧快照保持原名称，三个 view 样本均通过。

**热快照缩小不等于总数据量缩小。** 候选 V2 热快照与全部按需结果相加为 **122,024,211 bytes**，比基线完整冷快照 115,984,977 bytes 多约 **5.21%**。这是编码热快照加逻辑 JSON 载荷的总和，不是实际 HTTP 流量或存储 heap；它说明读取全部结果时不能声称总字节节省。单个 1 MiB 场景也另外读取 **1,048,634 bytes** 的完整结果 JSON。

## 流式通信：帧数改善，完整耗时回归

本报告全部毫秒值来自 `performance.now()` 的墙钟时间，不是操作系统统计的 CPU 使用时间。

候选设置：16 ms 定时器、64 KiB 待合并字节目标、256 updates 上限、4 MiB subscriber 预算。所有样本紧循环产生分片，由字节阈值刷新；记录到 266 次 flush 调用、265 次非空刷新，最后显式完成 flush 发送 1 帧。该场景不能证明任意真实到达节奏下的定时行为。

| 样本 | 基线端到端 ms | 候选端到端 ms | 基线 apply ms | 候选 apply ms | 候选 binary bytes | 候选等价 JSON bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 186.797 | 226.443 | 48.587 | 7.922 | 16788324 | 22408841 |
| 2 | 183.460 | 230.326 | 48.083 | 12.506 | 16788326 | 22408849 |
| 3 | 206.689 | 219.179 | 54.796 | 8.440 | 16788326 | 22408849 |

基线每次都是 **16,389 帧 / 17,703,875 V1 bytes / 25,189,286 JSON bytes**；候选都是 **265 帧**。以中位数计，帧数减少 **98.38%**，等价 demo JSON/base64 body 减少 **11.04%**，Yjs 二进制载荷减少 **5.17%**。候选 binary bytes 不包含未定义的二进制 envelope 序列化；JSON 字节与 demo adapter 的字段格式一致，两者都不含 SSE/HTTP/TCP 头。

**完整端到端中位数由 186.80 ms 增至 226.44 ms，增加 21.22%。** apply 由 48.59 ms 降至 8.44 ms，不能抵消其他路径成本。单次 accept 累加的 `projectionMs` 在候选中只包含 accept 内触发的同步 flush；因此主要结论使用包含收尾的 `endToEndMs`。

六个正式流样本均精确还原 **16,777,216 bytes**，公共视图终态 completed，SHA-256 都是 `732dd2b3f284d2273326b8acf57f014a895f513bfc79770eb963226aeaef8cb6`。这里没有真实 socket，也没有声称生产网络吞吐提升。

## 编码策略筛选附加实验

针对流式回归，主代理提供四种编码策略 probe。检查后先修正了 direct 仍调用单元素 merge、以及缺少最终全文断言的问题，再运行 4 × 3 个独立进程，全部 exit 0 / exact。可运行脚本（交付时改为相对导入并补齐类型收窄；实验原件保留在归档）：[codec-probe.ts](../../npm-packages/@peri-sdk/benchmarks/codec-probe.ts)；原始 stdout：[codec-probe.jsonl](raw/codec-probe.jsonl)；[运行记录/源码指纹](raw/codec-probe-provenance.json) 和 [源码归档](raw/codec-probe-source.tar.gz) 保留输入，采样期间源码指纹没有变化。

实际命令为 `/Users/konghayao/.bun/bin/bun /tmp/peri-yjs-codec-probe.ts <variant>`，variant 依次 direct、v2、convert、v1，各运行三次。脚本使用当前 SDK 产生 16 MiB chat text，单独比较编码/合并/应用成本，阈值仍为 64 KiB / 256 updates。

| 策略 | 三次 ms | 中位数 ms | 均值 ms | 样本标准差 ms | bytes | 帧 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| direct：每事件 V1 apply | 125.634 / 143.807 / 171.425 | 143.807 | 146.956 | 23.057 | 17166952 | 16386 |
| v2：每事件 V2，批量 mergeV2 | 215.236 / 217.401 / 203.844 | 215.236 | 212.160 | 7.283 | 16787958 | 265 |
| convert：V1 合并后一次转 V2 | 162.148 / 162.031 / 161.118 | 162.031 | 161.766 | 0.564 | 16787958 | 265 |
| v1：V1 批量合并 | 147.914 / 153.430 / 146.377 | 147.914 | 149.240 | 3.709 | 16975532 | 265 |

**观察**：convert 在这组筛选数据中比 v2 中位耗时低约 24.72%，输出载荷字节和帧数相同。direct 波动较大，不能由三次样本认定小幅差异显著。

**推断与边界**：每批合并后再编码 V2 值得进入下一轮正式验证。本 probe 未走 SessionDocSync、没有 session 文档复制、JSON adapter、网络、慢消费者或重连；其公开视图组合了复制 chat 和源 session，completed 只检查源状态。文本生成方式和计时边界也与正式 benchmark 不同。它仅筛选 chat codec 路径，不能替代完整端到端结果。全文独立 SHA-256 为 `ea73c87b58c9310621112efc326396d93c16ca2bab596643c1b0c065476aa5cd`。

## 描述统计、判断与下一步

正式候选三个进程的均值 / 中位数 / 样本标准差：

| 指标 | 均值 | 中位数 | 标准差 |
| --- | ---: | ---: | ---: |
| tools 投影 ms | 143.690 | 142.269 | 3.069 |
| tools 主快照 apply ms | 79.243 | 75.017 | 7.647 |
| view 128 次总 ms | 53.364 | 52.194 | 5.352 |
| stream 端到端 ms | 225.316 | 226.443 | 5.658 |
| stream apply ms | 9.623 | 8.440 | 2.510 |

观察支持工具索引与增量视图优化目标，也证明全文没有因外置而丢失。默认热快照的改善主要来自载荷隔离；同候选的 V1/V2 字节比较另显示 codec 的附加贡献。完整进程内存并非所有场景都下降，view 峰值仍增加约 10.3%；没有 heap profile，不能精确归因于缓存或载荷解析，也不能把 RSS 当成持久状态大小。

三次样本描述这台机器上的幅度，不能证明生产请求分布或小幅收益的统计显著性。采样顺序不是随机交错；增强公开断言与候选额外 V1 编码也使全场景工作多于第一轮，所以保留两组基线并明确主比较组。

下一步是实现筛选出的编码策略并重新运行正式流式与受影响场景。恢复、乱序/重复、慢消费者、取消、回放、compact/rewind、切换和销毁由相应 [sync](../../npm-packages/@peri-sdk/tests/session-sync.test.ts)、[payload](../../npm-packages/@peri-sdk/tests/tool-payloads.test.ts)、[view](../../npm-packages/@peri-sdk/tests/session-view-incremental.test.ts) 契约验证，不能从本吞吐实验推导。第二轮报告保留回归事实，等待独立复核及第三轮数据。
