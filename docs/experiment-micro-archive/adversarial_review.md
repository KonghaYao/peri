# 对抗验证：Micro archive 验证提交成本

## 总结

**CONCLUSION_STANDS，仅限当前综合报告明确界定的窄结论。** 尝试从数据来源、网络替代解释、计时混杂、认证闭环和内存范围推翻结论，未找到足以推翻资源调用数下降、canonical body 批次有界和保留单次原子提交的证据。稳定 wall、CPU、真实网络性能和总内存收益均未建立；第二轮较慢的结果必须保留。

本次独立审查未运行 Cargo、重跑测量或修改生产代码。输入为 [综合结论](conclusion.md)、[实验 1](01_experiment_report.md)、[验证 1](01_verification.md)、[实验 2](02_experiment_report.md)、[验证 2](02_verification.md)，以及两份报告嵌入的完整 stdout。另逐项核对原始 `/tmp/peri-micro-small-benchmark.log`、`/tmp/peri-micro-large-benchmark.log`、测量 harness、生产验证循环和 remote adapter。四个已记录源码 SHA256 均与本次审查快照一致。

## 攻击结果

### 攻击 1：把受控端口调用减少冒充真实网络加速

- **目标结论**：新路径减少验证的顺序资源 API 和推导远端批次数。
- **角度**：外部效度、替代解释。
- **攻击**：基准 body 直接从受控 canonical 字节映射返回，没有 Turso 网络或真实远端 body SQL 延迟。仅计数 fixture 的 `read_archive_chunks`，不足以证明真实 transport 聚合，更不能证明网络耗时收益。
- **核对**：报告明确区分实际资源 API、实现推导的 remote `read_batch` 和未测量的网络。生产 adapter 将一批查询交给一次 `read_batch`；该出口校验结果集数量并通过一致读事务执行。独立 adapter + SQLiteTransport 用例检查 32 块对应一次 transport 一致读且外会话读取失败。metadata API 内仍有两个顺序一致读，综合表采用 `2G+K → 2+ceil(K/32)`，未漏算它们。
- **严重程度**：若据此宣称真实网络加速则严重；对现有窄结论无新增缺陷。
- **处理**：保留“实现推导、非线上测量”。将来若要提出网络性能结论，需另测真实部署；本次没有据此推算 RTT 或节省秒数。

### 攻击 2：固定顺序、单样本和负向结果击穿性能因果链

- **目标结论**：完整路径测量提供了提交成本证据。
- **角度**：混杂变量、数据选择、逻辑链。
- **攻击**：两轮是两个不同负载，各策略每个负载只有一个样本，且固定 serial→batch。SQLite commit wall 第一轮为 287/57 ms，第二轮为 195/48 ms，缓存、调度或存储噪声可以解释部分差异。第二轮新路径 total wall 为 4377 ms，明显高于旧路径 2648 ms；总进程 CPU 也从 2345 ms 变为 2633 ms。
- **核对**：综合报告保留全部负向数据，未用第一轮 1113→888 ms 宣称普遍加速。第二轮新 planner wall 2961 ms、phase 进程 CPU 1242 ms，只说明存在 CPU 外时间，报告没有擅自认定原因。资源 port wall 可以重叠，未被相加为总耗时；整数毫秒截断也已披露。进程 CPU 包含同进程 writer/存储线程，未被冒充纯 planner CPU。
- **严重程度**：对“稳定端到端或 CPU 收益”足以否定；现有报告已明确不提出该结论。
- **处理**：保留负向观测及无法归因的判断。未来若需稳定耗时结论，应交替或随机顺序、多次重复，并分析 planner/commit 与真实远端环境。

### 攻击 3：测试基线和输入可能不对等

- **目标结论**：相同归档负载下，调用数下降且整个提交路径确实执行。
- **角度**：方法论、来源完整性。
- **攻击**：测试重建的 serial baseline 不是完整旧二进制；新路径新增 response-fit 和完整 member SHA256，不能把两者时间差简单当成批次本身的影响。异步 writer 若提前完成，所谓完整路径还可能漏计追加 I/O。
- **核对**：baseline 的旧 metadata/chunk 循环与修复前源码相符，复用当前 writer、planner、更新组装和 commit；新增检查的不对称已披露。append gate 放在真正 SQLite append 前，在 clock 开始后才放行。两策略 canonical 字节及 chunk 数分别完全一致：2234060/1500 与 16011768/3968；append 次数分别为 16 和 2，atomic commit 均为 1。源码指纹、命令、完整终态及 1000 step 积压超限的失败尝试均留档，没有把失败计入样本。
- **严重程度**：中等，限制时间归因；没有推翻实际调用计数和执行边界。
- **处理**：继续称为“旧 I/O 循环的受控复现”。若将来比较完整版本性能，应另建版本间对照；本次不需为窄结论补测。

### 攻击 4：fixture 自证，未证明跨 batch 原文认证

- **目标结论**：同等 canonical 原文经有界读取认证后，只发布一次投影。
- **角度**：真实性、遗漏负向位置。
- **攻击**：受控 body 与 planner 使用同一序列化来源，正例可能只能证明自洽；只有首块损坏用例也不能穷举跨 batch 边界或 member 尾部的故障。
- **核对**：受控字节只在真实 durable append 确认后开放。生产 `verify_chunk_batch` 检查响应数量、每块准确长度、total bytes、chunk digest；同一 SHA256 状态跨 batch 延续，只在 member 末尾 `finalize_reset` 并比较完整 digest。大结果每 member 超过一个 batch，成功提交确实经过该路径。另有真实 SQL adapter 的精确字节断言及 hot/cold 视图一致回归。认证失败和取消用例均要求零投影发布。
- **严重程度**：中等外部效度限制，未发现生产代码的认证漏洞。
- **处理**：结论仅声称受控路径和已审代码的认证闭环，不声称覆盖所有恶意损坏位置、真实远端故障或密码学证明。更强的抗故障声明需要额外针对性负例；当前报告未作该声明。

### 攻击 5：128 KiB 被误读为整体内存保证

- **目标结论**：canonical body 单批响应上界为 128 KiB，至多一个 batch 在飞。
- **角度**：遗漏指标、推论范围。
- **攻击**：`maximum_body_bytes` 记录查询 max_bytes 之和，不是 allocator/RSS，也不包括 catalog、transport framing、SDK 解码或字符串临时分配。
- **核对**：batch 类型限制至多 32 查询、每块至多 4096 bytes、同一 committed_tail；remote adapter 和认证出口另检查每块实际响应长度。生产循环 await 当前批次后再读下一批，没有 detached 验证任务。两轮记录最大在飞数 1 和最大查询预算 131072 bytes。报告明确排除 RSS/SDK 总分配及 metadata。
- **严重程度**：若推广到总内存则严重；现有 canonical body 范围成立。
- **处理**：保留 canonical body 单批限定。若以后需要总内存承诺，应另测分配或 RSS，不能复用本计数。

### 攻击 6：测量快照不等于最终代码快照

- **目标结论**：实验可作为最终修复的证据。
- **角度**：可追溯性。
- **攻击**：两轮结束后把单 query 边界校验归位到 `ArchiveChunkQuery::validate`，现有时间样本并未重新测量这份最终源码。
- **核对**：报告和综合结论明确披露这次归位；四个测量核心文件指纹仍匹配，归位保留原数量、字节、offset 及错误映射。该差异没有理由改变本次调用数公式，但不能把旧时间样本称为最终快照性能重测。
- **严重程度**：轻微，最终正确性检查仍需由交付统一验证提供。
- **处理**：保持披露；本对抗审查不代替主 agent 的最终 Rust 回归，也不据此修改已有时间数据。

## 综合评估

| 攻击 | 对当前结论的剩余影响 | 裁定 |
| --- | --- | --- |
| 受控端口与网络外推 | 线上性能未建立，remote 批次数仅实现推导 | 窄结论成立 |
| 单样本、顺序和负向结果 | 稳定 wall/CPU 收益未建立 | 窄结论成立 |
| 重建基线和异步写入 | 不支持批次的独立耗时归因 | 调用数与执行边界成立 |
| 自证与负向故障覆盖 | 不代表所有损坏位置或远端故障 | 受控认证闭环成立 |
| 查询预算与总内存 | 不支持 RSS/SDK 内存上界 | canonical body 上界成立 |
| 采样后规则归位 | 最终回归独立完成，性能未重采 | 已披露，不推翻窄结论 |

资源调用计数、canonical body 批次边界和现有代码的单次提交路径置信度高；实际 remote 批次数下降有实现与 adapter 回归支持，属于高置信度实现推导。稳定 wall、CPU、网络和总内存收益均无可成立的正向结论。

## 最终裁定

**CONCLUSION_STANDS。** 当前综合结论经受了挑战，其成立范围是减少顺序验证调用、限制单批 canonical body 并保留原文认证与原子发布。不得将本裁定扩大为已证明生产加速、总 CPU 降低或总体内存有界。

## 审查后快照差异

最终 staged review 后再次只读核对三项测试修正，未运行 Cargo 或重新采样：

1. `micro_batch_test.rs` 只给 `apply_serial_baseline` 增加 `#[cfg(unix)]`，与已有 Unix benchmark 调用方的编译条件一致，避免非 Unix strict Clippy 的未使用函数问题。移除此单行后 SHA256 恢复为原测量值 `436f9b0369264f688fcbd2b4b2ecbc83646cb9597dd34c2aaf7f4eedc3b66bc7`；新文件值为 `1ba39a1ddabf02163dcd7199726f49a52f2ea1a348a20ab7bff886aeac5c82b7`。macOS 下函数体、测量顺序、计时边界和执行路径完全相同；其余三个记录文件的 SHA256 仍未变化。前文“四个 SHA256 一致”描述的是初次审查时快照，原测量指纹及数字保留。
2. `stages/compact_test.rs` 的 `CompactFixtureTool` 显式声明 `is_direct=true`；`session/subagent/provenance_test.rs` 在 spawn/resume 夹具中显式注册 direct、ArchiveRead 的 `RecoveryTool`。两处让保留有损投影的测试声明所需恢复权限，其 invoke 仍会 panic，不能冒充实际恢复工具执行。它们不改变生产权限过滤，也不属于性能测量 harness。

这些差异不改变调用数、有界读取或原子发布的窄结论，裁定仍为 **CONCLUSION_STANDS**。本次只读复核不等于 Windows 编译验证，也不把原样本改称最终快照的性能重测；最终回归结果由交付验证另行记录。
