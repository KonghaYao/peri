# 实验 3：每批转换 V2 的最终候选

状态：**DONE**。2026-10-05（Asia/Shanghai）完成四个原有场景各 3 次独立采样，12/12 passed、exit 0，无预算截断；实际 SSE adapter 补充 3/3 passed。本文限于这些已完成场景；大历史叠加长流式的组合测试见 [实验 4](04_experiment_report.md)。

## 方法和来源

第二轮已观察到逐事件 V2 编码使纯流式完整耗时增加。最终候选改为收集 V1 事件、批量 mergeV1 后一次转换 V2，传输协议仍为 V2。本轮也包含 peer 缓冲区所有权、destroy 入口、root 替换缓存及 canonical 工具去重的修复；这些生命周期契约由独立测试验收，性能样本不替代它们。

源码于 `2026-10-04T16:51:20.523556Z` 冻结到 `/tmp/peri-yjs-sdk-final-round3`。源码 SHA-256：`96800d1a6c1be32cb460a8b85be68788a8410f5bc5215dd4eb3d8c5e16ebbb67`。Bun 1.4.2、Yjs 13.6.33、机器和锁文件不变；生成器 SHA-256 仍为 `5fe3c36d10ea9b148deb605559b4bb5724d78bfd9fb672ef134291cd01856a66`。

原四场景使用与第二轮相同的增强 harness，公开历史、完整载荷、真实工具失效和旧快照不变断言保持一致。stream 的 native 路径将 binary frame 直接应用，只额外计算等价 SSE JSON 字节；它没有基线已有的 base64 解码成本，**不能把 native 数字与基线直接比较为 demo 的整体加速**。因此另补 `stream-sse`：调用冻结 demo 的真实 `SessionDocStream`，执行实际 base64 编码/解码后再 apply，与基线相同方式计算 JSON body bytes。双方均不模拟 HTTP/SSE 网络、JSON.parse 或浏览器绘制。

```sh
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-final-round3 --api candidate \
  --output docs/experiment-yjs-sdk/raw/final \
  --scenarios tools,view,stream,large --samples 3
/Users/konghayao/.bun/bin/bun npm-packages/@peri-sdk/benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-final-round3 --api candidate \
  --output docs/experiment-yjs-sdk/raw/final-sse \
  --scenarios stream-sse --samples 3
```

复现请改用未存在的输出目录。原四场景的 [summary](raw/final/summary.json)、[provenance](raw/final/provenance.json)、[源码归档](raw/final/frozen-source.tar.gz) 和 [当时 harness 归档](raw/final/harness.tar.gz) 均保留。SSE 补充的 [summary](raw/final-sse/summary.json) 与 [provenance](raw/final-sse/provenance.json) 指向扩展后的 harness；解包、锁定依赖与运行方法同 [实验 2](02_experiment_report.md)。

## 原有场景的最终结果

下表使用三个独立进程的中位数；基线来自第二轮的增强 harness / 流式补采。

| 指标 | 基线 | 最终候选 |
| --- | ---: | ---: |
| 12k 工具投影 ms | 1740.883 | 143.534 |
| V1 热快照 bytes | 115984977 | 24719251 |
| 主编码热快照 bytes | 115984977（V1） | 23059321（V2） |
| 主快照 encode ms | 175.212 | 68.609 |
| 主快照 apply ms | 118.796 | 79.835 |
| 128 次视图总耗时 ms | 2883.206 | 50.347 |
| 视图单次中位数 ms | 22.101 | 0.338 |
| 视图经验 p95 ms | 24.734 | 0.637 |
| 未改变工具身份复用 | 0/128 | 128/128 |
| tools 峰值 RSS MiB | 1456.53 | 1289.75 |
| view 峰值 RSS MiB | 971.70 | 1074.06 |
| 1 MiB 结果热快照 bytes | 1051095 | 2779 |

候选各样本的工具投影为 **160.473 / 135.786 / 143.534 ms**；view 128 次总耗时为 **49.992 / 50.363 / 50.347 ms**。工具投影中位耗时约降低到基线的 1/12.13，view 单次中位耗时约为基线的 1/65.41；这些倍数限于本机确定性工作量。view 峰值 RSS 仍增加约 **10.53%**，没有把缓存优化表述为所有内存指标都下降。

三份工具冷副本都校验 12,000 个工具、1,200 个历史 assistant、完整输入与结果、顺序、归属和 completed。每份额外读取 **98,964,890 bytes** 的完整载荷；每份 view 都通过真实工具更新失效与旧快照不变检查。1 MiB 结果另读取 **1,048,634 bytes**。热快照加全体按需载荷仍为 **122,024,211 bytes**，比原完整快照约多 5.21%；收益是默认同步隔离，不是读取全部全文的总字节减少。

## 纯流式：实际 SSE 路径接近基线

| 样本 | 最终 native 端到端 ms | 最终实际 SSE 端到端 ms | 基线 SSE 端到端 ms | 最终 SSE apply ms |
| --- | ---: | ---: | ---: | ---: |
| 1 | 169.106 | 184.435 | 186.797 | 10.647 |
| 2 | 173.989 | 180.902 | 183.460 | 10.200 |
| 3 | 168.826 | 176.971 | 206.689 | 8.954 |
| 中位数 | 169.106 | 180.902 | 186.797 | 10.200 |

**公平的 demo 路径比较为 186.80 → 180.90 ms，约少 3.16%，应判断为接近基线。** 样本只有三个且未随机交错，不能据此声称显著加速；这里测的是墙钟时间。原 native 中位数 169.11 ms 仅作为二进制路径的独立指标；与第二轮相同 native 路径的 226.44 ms 相比降低约 25.32%，支持每批转换的优化方向。

最终所有流样本均为 **265 帧 / 16,788,326 V2 bytes / 22,408,849 JSON body bytes**；基线为 **16,389 帧 / 17,703,875 V1 bytes / 25,189,286 JSON body bytes**。即帧数少 **98.38%**，等价 JSON body 少 **11.04%**。显式完成 flush 发送最后 1 帧；所有样本最终公共视图均为 completed，16,777,216 bytes 全文 SHA-256 等于 `732dd2b3f284d2273326b8acf57f014a895f513bfc79770eb963226aeaef8cb6`。

native 与 SSE 由输出目录、`framing`/计时说明及各自归档脚本区分；SSE 补充另有明确的 `transport` 字段。SSE 补充沿用了通用 `framing` 描述来说明二进制载荷与 JSON 字节计数；实际是否编解码以 `transport` 字段和归档脚本为准。这里的 JSON 字节不包含 HTTP/SSE/TCP framing，也没有真实网络吞吐测量。

## 描述统计与结论边界

| 最终指标 | 均值 | 中位数 | 样本标准差 |
| --- | ---: | ---: | ---: |
| 工具投影 ms | 146.598 | 143.534 | 12.625 |
| 主快照 apply ms | 80.818 | 79.835 | 5.668 |
| view 128 次总 ms | 50.234 | 50.347 | 0.210 |
| native 流式端到端 ms | 170.640 | 169.106 | 2.903 |
| 实际 SSE 流式端到端 ms | 180.769 | 180.902 | 3.734 |

本轮观察支持大历史工具投影、默认热同步和增量视图目标。纯流式实际 SSE 耗时接近基线，帧数明显减少；不以 native 路径少做解码的数字夸大收益。完整进程 RSS 包含源文档、编码/解码缓冲、冷副本、视图与全文校验，不能解释为常驻状态大小。组合风险另由 [实验 4](04_experiment_report.md) 检查，恢复及销毁等语义另由契约测试与独立验证确认。
