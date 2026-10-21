# Micro archive 验证提交成本：综合结论

状态：两轮实验及独立验证完成；[最终对抗审查](adversarial_review.md)结论为CONCLUSION_STANDS（仅限下述范围）。本目录只记录测量，不替代 [Micro 设计](../design/micro-compact.md)。

## 确定的结果

1. 新生产路径在canonical writer barrier之后只固定一次committed_tail，所有catalog/member/body batch共用该尾界。每批至多32×4096 canonical bytes，至多一个body batch在飞；远端在同一read_batch事务读取整批，不在单SDK连接上并发启动事务。
2. 逐chunk长度和SHA256认证保留，并增加跨batch的完整member SHA256认证。全部验证成功后沿原有单次atomic projection commit发布；取消会丢弃当前future，没有脱离调用方的验证任务。
3. 两轮完整路径执行均得到一次atomic commit，canonical验证字节数一致；API次数与`ceil(K/32)`精确一致。单纯planner benchmark不能提供这些结论，本目录测量明确包含barrier、验证和commit。

| 负载 | catalogs / chunks / canonical bytes | metadata API旧→新 | body API旧→新 | remote read_batch推导旧→新 |
| --- | --- | --- | --- | --- |
| 500×4096 bytes结果 | 500 / 1500 / 2234060 | 500→1 | 1500→47 | 2500→49 |
| 32×500000 bytes结果 | 32 / 3968 / 16011768 | 32→1 | 3968→124 | 4032→126 |

最后一列由真实remote adapter的metadata每API两批、body每API一批推导，**没有测量真实Turso网络请求**。实际adapter + SQLiteTransport回归独立断言32个4KiB块恰好一个read_batch、外会话读取拒绝。

## 支持这些结果的验证

- [第一轮实验](01_experiment_report.md)、[第一轮独立验证](01_verification.md)、[第二轮实验](02_experiment_report.md)、[第二轮独立验证](02_verification.md)含命令、完整stdout、输入指纹、退出码和局限；两个手动benchmark各1 test passed。
- `micro::batch_tests`正确性预验3passed、2benchmark ignored：真实flush/commit与受控body port闭环，固定tail、全计划批次数、hot/cold视图相同、认证失败零发布、取消丢弃在飞读取。
- `archive_remote_reads_32_chunks_in_one_bounded_transaction`：实际remote adapter + SQLiteTransport，1passed；`context_archive::batch::tests`：空批次、数量/字节/offset越界及尾界不一致，1passed。
- 测量后单query边界规则收敛至`ArchiveChunkQuery::validate`，范围和资源InvalidInput/detail映射保持；该纯规则归位后的最终快照交统一Rust验证，不以测量前单测替代最终检查。

## 各项结论的置信度

| 结论 | 置信度 | 支持范围 |
| --- | --- | --- |
| metadata API为1、body API为ceil(K/32) | 高 | 两轮实际计数与生产循环一致 |
| canonical body每批至多128KiB、至多1批在飞 | 高 | 验证过的批次类型、SQL substr边界与受控读取测试；不包含RSS/SDK元数据 |
| 完整验证后单次atomic commit，认证失败/取消零发布 | 高 | 真实SQLite提交与受控失败/取消回归；最终快照继续由统一验证覆盖 |
| remote read_batch数量由2G+K降为2+ceil(K/32) | 高，属于实现推导 | metadata代码与32块一次read_batch的真实adapter回归；并非线上网络观测 |
| 稳定wall、总CPU、网络吞吐/延迟收益 | 未建立，不给正向置信度 | 固定顺序单样本、受控body port、没有真实Turso或重复采样 |

## 可以和不可以推断什么

可以确认：相同数量的原文块通过更少的顺序资源API/推导远端批次完成认证，body canonical响应上界为128KiB，单连接不新增事务并发，atomic发布边界保留。

不能确认：稳定端到端延迟、真实Turso吞吐或RTT、总体CPU收益、RSS/SDK总分配上界。body port是受控字节端口；metadata/flush/commit是本机真实SQLite，不能冒充线上远端I/O。

两轮都是旧→新固定顺序、单样本。小结果total wall旧1113ms/新888ms，大结果旧2648ms/新4377ms。总进程CPU小结果旧856ms/新847ms，大结果旧2345ms/新2633ms；第二轮新planner wall2961ms而phase进程CPU1242ms，存在明显CPU外时间。commit port wall差异也大（第一轮287/57ms、第二轮195/48ms），没有重复采样/缓存隔离/调度分析，不能把任何总耗时差直接归因于批次优化。新路径增加完整member digest及response-fit检查，CPU差值未做profiler归因。

fixture在计时前构造对象，但append gate保证真正SQLite追加在clock开始后发生；资源port累计wall可能与phase重叠，不可简单相加。初始1000step因真实1024条积压保护失败，没有性能数据；经批准改为500step，生产预算和75%/95%/45%规则均未修改。

## 后续

两轮独立验证均PASS；综合结论已通过[独立对抗审查](adversarial_review.md)，仅支持本文列明的窄结论。实际Turso部署可用后，另行运行有多次采样/顺序控制的网络测量；现有结果不承诺生产延迟。

## 审查后快照差异

最终 staged review 后，测试 harness 的 `apply_serial_baseline` 增加一行 `#[cfg(unix)]`，对齐仅 Unix 的 benchmark 调用方，避免非 Unix strict Clippy 的未使用函数问题。新 harness SHA256 为 `1ba39a1ddabf02163dcd7199726f49a52f2ea1a348a20ab7bff886aeac5c82b7`；移除此单行即恢复两轮报告中的原测量 SHA256 `436f9b0369264f688fcbd2b4b2ecbc83646cb9597dd34c2aaf7f4eedc3b66bc7`。macOS 测量的函数体和执行路径相同，其他三个记录文件指纹未变化；原样本、原指纹和计数均保留，没有重新采样。

另外两处测试夹具权限修正：`stages/compact_test.rs` 显式声明 fixture 工具为 direct；`session/subagent/provenance_test.rs` 的 spawn/resume 显式注册 direct、ArchiveRead 的恢复工具夹具。它们声明测试所需权限，未改变生产授权规则或性能测量路径。[对抗复核](adversarial_review.md)确认上述差异不推翻窄结论；只读审查不替代最终 Rust 回归或 Windows 编译，也不构成最终快照的性能重测。
