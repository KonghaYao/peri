# 验证报告 2：大结果跨批次的 Micro 原文认证

## 总体评估

**PASS（限于受控路径的认证、调用次数、有界性与单次提交）。** 可以编写综合结论。本轮新路径 total wall 更长，两轮均不能支持稳定延迟、吞吐、总 CPU 或总内存收益。

独立审查 [实验报告](02_experiment_report.md)、原始 `/tmp/peri-micro-large-benchmark.log`、测量 harness、生产验证循环、batch 类型和 remote adapter；未重复实验，也未运行 Cargo。报告四个核心文件的 SHA256 均与审查时一致。测量后的 query 校验归位已经披露，本报告不将已有时间样本视作最终代码快照的重新测量。

## 逐维度评估

### 数据完整性：PASS

1. 原始 stdout 为 **1 passed / 0 failed**，完整包含两个策略观测，与报告逐项一致。两者均为 32 catalog、64 affected rows、3968 chunks、16011768 canonical bytes；各有 2 次真实 append、1 次 atomic commit。
2. `ceil(3968 / 32) = 124`，对应 body API 实测 `3968 → 124`；metadata API 为 `32 → 1`。两者最大在飞 body batch 均为 1，新路径最大请求 canonical 字节上界为 `32 × 4096 = 131072`。
3. planner、apply、total 的 wall 与进程 CPU，以及各 port 累计 wall 均已记录。新 planner 为 wall 2961 ms、CPU 1242 ms；报告明确保留异常差距，未编造其原因。metadata 0 ms 为整数截断，phase 求和至多相差 1 ms 也与独立采样和截断相符。
4. 500000 个 ASCII 字符明确使用十进制 bytes；canonical envelope 的实际体积单独测量，没有把正文尺寸当作 canonical 尺寸。输入总量低于生产默认 16 MiB，未通过提高预算回避积压约束。

### 方法正确性：PASS

1. 真正 SQLite append 仍在计时开始后由 gate 放行；随后执行 planner、flush barrier、验证与 atomic commit。fixture 构造、编译排除在计时外，真实追加 I/O 包含在完整路径中；异步 writer 的 CPU 归入相应进程 phase，port wall 与 phase 可重叠。
2. 大结果单 member 超过 32 个 4096-byte chunk，必然跨多个 batch。`verify_catalogs` 把同一 `Sha256` 状态传入每次 `verify_chunk_batch`；逐块校验长度和 chunk digest，只有到达 member 的完整 canonical 尾界才 `finalize_reset` 并核对 member digest。batch 边界既不清空摘要，member 边界也不混合摘要。本次成功进入 commit，结合该代码路径，支持有效大 member 的跨 batch 认证闭环；不能据此声称已覆盖所有恶意损坏位置。
3. 每批只 await 一个资源请求，所有 query 共享同一 committed tail。batch 类型限制至多 32 块、每块至多 4096 bytes；本轮计数与该边界一致。128 KiB 表示 canonical body 响应上界，不包含 metadata、SDK 分配或进程 RSS。
4. body 为受控 port；metadata、append、commit 为真实 SQLite。remote adapter 每个 metadata API 先读取 tail、再读取 bounds/metadata，body batch 则调用一次 `read_batch`，所以 `2 × 32 + 3968 = 4032` 到 `2 + 124 = 126` 是实现推导，报告正确地没有将其写成真实 Turso 测量。
5. 固定旧→新顺序、各一次，且 baseline 未执行新增 response-fit 和完整 member SHA256，不能形成无混杂的性能因果比较。报告已清楚说明这些不对称，实验仍适合核对调用次数和完整提交流程。

### 结论合理性：PASS

1. 新 total wall **4377 ms**、旧 **2648 ms**，总 CPU 新 **2633 ms**、旧 **2345 ms**。报告如实呈现，未将 API 次数减少直接等同于全路径加速。
2. commit port wall **195 → 48 ms** 与大幅 planner wall/CPU 差距均为替代解释或混杂；新路径额外认证也可能影响 CPU，但没有 profiler，不能把全部 CPU 差值归因于它。报告保留这些限制。
3. 支持的结论限于本次同等 canonical 字节经有界 batch 完成认证并各提交一次，以及调用数公式；真实网络、重复采样后的稳定表现、吞吐和整体内存仍未测量。

### 报告质量：PASS

方法、原始数据、完整终态、异常解释范围、输入指纹和后续代码变动均可追溯。事实观测、实现推导和未测量项区分明确，与第一轮口径一致。

## 关键问题

未发现阻断综合结论的问题。性能证据的边界仍是单次固定顺序、受控 body port 和变化较大的非验证 phase；综合报告必须保留第二轮变慢的观测。测量后 query 校验归位的最终正确性由主 agent 的统一 Rust 验证负责，本次只核对其规则与错误映射未改变。

## 改进建议

1. 综合结论同时列出两轮的实际 API 次数和 total wall，保留第二轮负向结果；推荐依据是减少串行资源调用并保持有界认证，不能写成已证明生产加速。
2. 将 remote transport 批次数注明为实现推导，并保留独立 adapter 回归与真实 Turso 测量的区别。
3. 若以后需要稳定性能结论，另做交替或随机顺序的重复采样、planner/commit 分析及真实远端测量；若需要更强的认证抗损坏结论，补充跨 batch 边界及 member 尾部的负向用例。本次无需为缩小结论范围而重跑实验。

## 上一轮反馈的处理情况

第一轮要求的大 member、实际 chunk 数、append gate、生产默认预算、phase/port 分离以及 commit 混杂均已处理。SHA 状态跨 batch 延续且按 member 重置经独立代码审查确认；本轮出现 wall 变慢后保留原始观测并缩小结论，没有挑选性删除不利结果。
