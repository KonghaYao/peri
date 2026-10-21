# 验证报告 1：大量小结果的 Micro 归档批量验证

## 总体评估

**PASS（限于调用次数、canonical body 批次上界与完整提交路径）。** 允许进入第二轮。未验证真实 Turso 网络延迟、吞吐或进程总内存；本次 wall 差异不能解释为批处理的确定加速比例。

验证方式：独立只读审查 [实验报告](01_experiment_report.md)、原始 `/tmp/peri-micro-small-benchmark.log`、测量 harness、生产验证循环、batch 类型与 remote adapter；未重复执行实验，也未在测量窗口运行 Cargo 任务。报告记录的四个源码 SHA256 均与审查时文件一致。

## 逐维度评估

### 数据完整性：PASS

1. stdout 终态为 **1 passed / 0 failed**，与报告及精确命令一致；同一测试中的两个策略观测均完整。500 catalog、1500 chunks、2234060 canonical bytes、1000 affected rows，两策略一致；16 次 append、1 次 atomic commit 均有记录。
2. `ceil(1500 / 32) = 47`，与新 body API 实测一致；metadata API 为 `500 → 1`。最大在飞批数均为 1，请求 canonical 字节上界为 `4096 → 131072`。这些是查询预算和调用计数，报告没有把它们写成 RSS 或 SDK 分配峰值。
3. planner、apply、total 的 wall 与 phase 进程 CPU，以及各资源 port 累计 wall 均已采集。至多 1 ms 的 phase 求和差异可由独立采样与整数毫秒取整解释。
4. 1000 step 的积压超限和 E0603 编译尝试明确列为失败，未计入有效测量；有效 500 step 保留生产默认预算。

### 方法正确性：PASS

1. `make_fixture` 时 append gate 已关闭；真正的 SQLite append 在 gate 后。wall/CPU clock 开始后才放行，再执行 planner、生产 flush barrier、验证与 atomic commit，修复了此前可能漏计异步 append 的计时边界。
2. baseline 仅替换旧验证循环，其余 planner、写入和提交共用现有代码；新策略执行生产 `apply_micro_plan`。新策略额外执行 response-fit 和完整 member SHA256 核验，报告已披露这个不对称，不将其冒充完整旧二进制对照。
3. body 是受控字节端口，metadata/append/commit 为真实 SQLite。报告正确区分资源 API 次数、remote adapter 的 `read_batch` 次数及真实网络；`RemoteSessionData` 每次 metadata 读取仍含 tail 与 bounds/metadata 两个顺序请求，因此远端批次推导 `2500 → 49` 与实现相符。
4. 固定旧→新顺序、各一次，仍有缓存和调度混杂。尤其 SQLite commit port wall 为 287 ms 与 57 ms；该变化不能由验证批次数直接解释。报告对此保留限制，未据此推出生产加速比例。

### 结论合理性：PASS

1. 本轮直接支持“同等归档字节、一次提交的前提下，验证 API 次数降为有界批次数”。生产循环把整个计划固定在同一 tail，并只等待一个最多 32 块的批次；测量与该规则一致。
2. remote adapter 的 32 块合成 transport 回归证明 SQL 请求的聚合接线，但不能替代真实 Turso 网络观测；报告没有跨越这一边界。
3. total wall 1113 ms 与 888 ms、进程 CPU 856 ms 与 847 ms 只作本次描述，未声称均值、显著性或普遍性能优势，结论范围合理。

### 报告质量：PASS

计划、方法、原始输出、失败尝试、输入指纹和局限均可追溯。事实、实现推导和未测量项区分清晰。

## 关键问题

未发现阻断第二轮的问题。下列限制必须继续保留：单次固定顺序不能证明稳定 wall 优势；受控 body 端口不能证明真实网络延迟；128 KiB 仅约束 canonical body 的单批请求/响应，不代表总内存。

## 改进建议

1. 第二轮使用 32 条 500000 字节结果，保持同一 gate、默认预算与串行命令，验证单 member 跨多个 batch 时摘要状态正确延续；记录实际 chunk 数，不从正文大小直接猜测 canonical envelope 大小。
2. 继续分别列 phase wall/进程 CPU 与 port 累计 wall；这些时间可以重叠，不能求和充当总耗时。保留 commit wall 的混杂说明。
3. 综合结论以调用次数公式和有界性为主；若今后需要声称稳定性能优势，再补顺序交替、多次重复与真实远端测量。

## 上一轮反馈的处理情况

预审提出的两点已处理：append gate 将全部真实追加 I/O 纳入计时；metadata API 与两个实际 remote 请求已明确区分。1000 step 触及真实积压上限后改为 500 step，未放宽生产阈值或开放测试专用 API。
