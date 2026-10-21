# 实验 1：大量小结果的完整 Micro 提交路径

## 基本信息

- 日期：2026-10-21；状态：DONE_WITH_CONCERNS（单次受控观测，不是生产延迟证据）。
- 假设：K个原文块由K次body API降为ceil(K/32)，metadata API由每catalog一次降为全计划一次；真实性和单次原子提交不变。
- 环境：macOS 26.5.1（25F80），arm64；rustc 1.99.0（b940084d7），cargo 1.99.0（5f94df478）；未优化test profile + debuginfo，CARGO_INCREMENTAL=0。
- 代码基点：`1f730e2453f5b476a690ced0814a015b9a22afd5` 加当前修复工作区；输入SHA256见下。未使用真实模型或Turso网络。

## 实验方法

1. 500个Read步骤、每条结果4096个ASCII正文字符；加上Human和AI调用，共1001条canonical消息。两策略结构与尺寸相同、各自新建临时SQLite会话，UUID不同。`micro_compact_stale_steps=0`仅用于对所有已曝光且已闭合步骤施加相同测量负载，不改变生产默认。
2. fixture对象构造不计时；append gate挡在真正SQLite append之前，开始wall/CPU计时后放行，再执行planner→flush barrier→验证→一次atomic projection commit。使用生产默认待持久化预算。
3. `serial_baseline`只在测试中复现旧的逐catalog metadata、逐4KiB chunk等待；writer/barrier、prepare_updates、最终atomic commit复用当前生产实现。`bounded_batch`直接调用生产`apply_micro_plan`。
4. baseline没有新增的fragment response-fit检查及完整member SHA256增量核验，仍包含原来的每chunk digest/total_bytes核验；新策略增加这两项检查。因此它是旧I/O循环的受控复现，不能当作完整旧commit二进制对照。
5. metadata、append、commit走真实SQLite资源门面；body走受控remote byte port，仅在真实append确认后接纳canonical bytes。计数是**实际资源API调用数**；受控body port没有注入网络RTT。
6. 顺序固定为旧→新，各测一次；目标串行运行，运行前没有其他本worktree的Cargo测试。未隔离操作系统其他进程；没有均值/方差或显著性结论。进程CPU按phase取getrusage差值，可能包含同进程writer/存储线程，不能称为纯planner CPU。资源port wall为调用累计，可能与phase重叠，不能与phase简单相加。

命令（exit **0**；实际 **1 test passed**，其中包含两个策略观测）：

```bash
CARGO_INCREMENTAL=0 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib agent::compact_v2::micro::batch_tests::micro_complete_path_small_results_benchmark -- --ignored --exact --nocapture --test-threads=1
```

## 原始数据

| 指标 | 旧串行复现 | 新有界批次 |
| --- | ---: | ---: |
| catalog / affected rows | 500 / 1000 | 500 / 1000 |
| chunks / canonical bytes | 1500 / 2234060 | 1500 / 2234060 |
| metadata API调用 | 500 | 1 |
| body batch API调用 | 1500 | 47 |
| 最大在飞body批次 | 1 | 1 |
| 最大请求canonical字节上界 | 4096 | 131072 |
| 真实append / atomic commit调用 | 16 / 1 | 16 / 1 |
| planner wall / phase进程CPU（ms） | 284 / 281 | 290 / 286 |
| apply wall / phase进程CPU（ms） | 829 / 574 | 597 / 561 |
| total wall / 进程CPU（ms） | 1113 / 856 | 888 / 847 |
| SQLite append port wall（ms） | 101 | 102 |
| SQLite metadata port wall（ms） | 239 | 1 |
| 受控body port wall（ms） | 13 | 5 |
| SQLite commit port wall（ms） | 287 | 57 |

`maximum_body_bytes`记录每批查询max_bytes之和；它证明canonical响应上界，不是RSS/SDK总分配测量。Remote adapter移交Blob所有权，不额外复制一整批canonical body。

完整成功输出：

```text
   Compiling peri-agent v0.2.0 (/Users/konghayao/code/ai/peri/.worktrees/micro-compact-redesign/peri-agent)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 14.93s
     Running unittests src/lib.rs (target/debug/deps/peri_agent-d1381109b0a3e95b)

running 1 test
test agent::compact_v2::micro::batch_tests::micro_complete_path_small_results_benchmark ... MICRO_COMPLETE_PATH case=small_results strategy=serial_baseline steps=500 body_bytes=4096 catalogs=500 affected=1000 chunks=1500 canonical_bytes=2234060 planner_wall_ms=284 planner_process_cpu_ms=281 apply_wall_ms=829 apply_process_cpu_ms=574 total_wall_ms=1113 total_process_cpu_ms=856 metadata_api_calls=500 body_batch_api_calls=1500 maximum_in_flight_batches=1 maximum_body_bytes=4096 append_calls=16 commit_calls=1 sqlite_append_port_wall_ms=101 sqlite_metadata_port_wall_ms=239 controlled_body_port_wall_ms=13 sqlite_commit_port_wall_ms=287 network_rtt=not_measured
MICRO_COMPLETE_PATH case=small_results strategy=bounded_batch steps=500 body_bytes=4096 catalogs=500 affected=1000 chunks=1500 canonical_bytes=2234060 planner_wall_ms=290 planner_process_cpu_ms=286 apply_wall_ms=597 apply_process_cpu_ms=561 total_wall_ms=888 total_process_cpu_ms=847 metadata_api_calls=1 body_batch_api_calls=47 maximum_in_flight_batches=1 maximum_body_bytes=131072 append_calls=16 commit_calls=1 sqlite_append_port_wall_ms=102 sqlite_metadata_port_wall_ms=1 controlled_body_port_wall_ms=5 sqlite_commit_port_wall_ms=57 network_rtt=not_measured
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1284 filtered out; finished in 2.51s


```

## 分析结果

1. `ceil(1500/32)=47`，新body API与预期精确一致；metadata由500次变成1次，原文验证字节一致，最终仍一次atomic commit。
2. 真实RemoteSessionData的metadata一次仍含两个顺序read_batch。由该实现推导，远端验证批次数为旧`2×500+1500=2500`、新`2+47=49`；**这是实现推导，完整基准没有实际调用Turso transport**。另一个真实adapter + SQLiteTransport回归已执行1例，断言32个4KiB body chunks恰好一次read_batch，外会话仍NotFound。
3. 本次total wall/CPU仅描述该次本机观测。commit port wall差异明显（287ms与57ms），固定旧→新顺序和单样本无法排除缓存/调度差异，不能把总耗时差全部归因于批次，也不推算生产网络秒数。

## 结论

在此负载下，验证API调用数和128KiB canonical body上界符合假设。完整planner→barrier→verify→atomic commit确实执行；未证明真实远端延迟、吞吐或总内存上界。

## 异常与发现

- 首次1000step尝试未产生性能数据：2001条消息在append gate关闭期间超过生产默认1024条待持久化上限，exit101、0passed。错误为`transcript persistence backlog is full (1024 item(s)/16777216 byte(s) outstanding)`。经主agent调整为500step（见00_plan），未改变生产预算。
- 尝试引用现有with_persistence_budget时，参数类型隐藏导致E0603编译失败；已删除该尝试，没有暴露新API或更改类型可见性。编译失败不计入测量。
- fixture正确性预验：3passed、2benchmark ignored，覆盖固定tail、全计划分批、cold/hot视图一致、认证失败零发布及取消丢弃在飞batch。真实remote批次回归1passed；类型batch边界回归1passed。

## 输入指纹

- `peri-agent/src/agent/compact_v2/micro_batch_test.rs`: `436f9b0369264f688fcbd2b4b2ecbc83646cb9597dd34c2aaf7f4eedc3b66bc7`
- `peri-agent/src/agent/compact_v2/micro.rs`: `6c3099a777f20d3baf6ba95af6eced49606f887d6b1b13d020bcb8cd2dad1328`
- `peri-agent/src/context_archive.rs`: `5fb8d0de884a9b5b950d54bfc180a938efbc6ddff7a41e3ac791aef380f63b5b`
- `peri-resources/src/sessions/remote/session_archive.rs`: `98de37b46ec55f8bb2fd7b4945a4b8ea304968b81dd3372cf70be26825094c1d`

## 下一步建议

独立复核方法和原始计数后，再以32条500000字节结果验证跨member/跨batch的大正文路径；不增加网络RTT假设。

采样后的最终复核另给仅Unix benchmark使用的 `apply_serial_baseline` 添加 `#[cfg(unix)]`，避免Windows严格检查产生未使用函数告警；Mac测量函数体和执行路径未改变，没有重采样。上列harness SHA保留为采样快照；交付harness SHA为 `1ba39a1ddabf02163dcd7199726f49a52f2ea1a348a20ab7bff886aeac5c82b7`。其他三个测量核心文件指纹保持。
