# 实验 2：少量大结果的完整 Micro 提交路径

## 基本信息

- 日期：2026-10-21；状态：DONE_WITH_CONCERNS（调用数假设成立；单次wall观测存在明显噪声）。
- 假设：单member跨多个有界batch时仍完成所有原文认证，K个body读取降为ceil(K/32)，并且只发布一次投影。
- 环境同第一轮：macOS 26.5.1 arm64，rustc/cargo 1.99.0，未优化test profile + debuginfo，CARGO_INCREMENTAL=0；基点`1f730e2453f5b476a690ced0814a015b9a22afd5`加修复工作区。
- 前序反馈：[01_verification.md](01_verification.md)允许继续，要求保持append gate/默认预算/串行、检查跨batch SHA状态，并持续披露固定采样顺序和commit时间混杂。

## 实验方法

1. 32个Read步骤，每条结果500000个ASCII正文字符（500000 bytes，非精确500KiB），共65条canonical消息。新旧两策略结构与尺寸相同、各自新建临时SQLite会话，UUID不同。
2. fixture构造在计时外，原文append gate在wall/进程CPU计时开始后放行，使用生产默认1024条/16MiB预算。随后执行相同planner→flush barrier→verify→一次atomic commit；实际归档内容16011768 bytes，加Human后仍未超过默认字节预算。
3. 保持第一轮的test-only旧I/O循环与新生产`apply_micro_plan`，`micro_compact_stale_steps=0`相同。旧baseline不包含新增fragment-fit和完整member SHA256；新实现保留每chunk digest并跨batch增量核验完整member digest。
4. metadata、flush和commit走真实SQLite门面；body为受控remote byte port。计数是实际资源API次数，未访问真实Turso。最大字节值是查询max_bytes之和，表示canonical响应上界，非RSS或SDK总内存。
5. 仍按旧→新固定顺序各测一次，单测试线程；编译不计时，不进行RTT注入。进程CPU分phase含同进程线程；port wall可与phase重叠，不能简单相加。未隔离整台机器其他进程。

命令（exit **0**，实际 **1 test passed**，包含两个策略观测）：

```bash
CARGO_INCREMENTAL=0 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib agent::compact_v2::micro::batch_tests::micro_complete_path_large_results_benchmark -- --ignored --exact --nocapture --test-threads=1
```

## 原始数据

| 指标 | 旧串行复现 | 新有界批次 |
| --- | ---: | ---: |
| catalog / affected rows | 32 / 64 | 32 / 64 |
| chunks / canonical bytes | 3968 / 16011768 | 3968 / 16011768 |
| metadata API调用 | 32 | 1 |
| body batch API调用 | 3968 | 124 |
| 最大在飞body批次 | 1 | 1 |
| 最大请求canonical字节上界 | 4096 | 131072 |
| append / atomic commit调用 | 2 / 1 | 2 / 1 |
| planner wall / phase进程CPU（ms） | 1064 / 1061 | 2961 / 1242 |
| apply wall / phase进程CPU（ms） | 1584 / 1283 | 1415 / 1390 |
| total wall / 进程CPU（ms） | 2648 / 2345 | 4377 / 2633 |
| SQLite append port wall（ms） | 258 | 274 |
| SQLite metadata port wall（ms） | 84 | 0 |
| 受控body port wall（ms） | 9 | 2 |
| SQLite commit port wall（ms） | 195 | 48 |

0ms为整数毫秒截断，不表示没有执行I/O。各phase独立截断，CPU相加可能相差1ms。

完整输出：

```text
   Compiling peri-agent v0.2.0 (/Users/konghayao/code/ai/peri/.worktrees/micro-compact-redesign/peri-agent)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 15.33s
     Running unittests src/lib.rs (target/debug/deps/peri_agent-d1381109b0a3e95b)

running 1 test
test agent::compact_v2::micro::batch_tests::micro_complete_path_large_results_benchmark ... MICRO_COMPLETE_PATH case=large_results strategy=serial_baseline steps=32 body_bytes=500000 catalogs=32 affected=64 chunks=3968 canonical_bytes=16011768 planner_wall_ms=1064 planner_process_cpu_ms=1061 apply_wall_ms=1584 apply_process_cpu_ms=1283 total_wall_ms=2648 total_process_cpu_ms=2345 metadata_api_calls=32 body_batch_api_calls=3968 maximum_in_flight_batches=1 maximum_body_bytes=4096 append_calls=2 commit_calls=1 sqlite_append_port_wall_ms=258 sqlite_metadata_port_wall_ms=84 controlled_body_port_wall_ms=9 sqlite_commit_port_wall_ms=195 network_rtt=not_measured
MICRO_COMPLETE_PATH case=large_results strategy=bounded_batch steps=32 body_bytes=500000 catalogs=32 affected=64 chunks=3968 canonical_bytes=16011768 planner_wall_ms=2961 planner_process_cpu_ms=1242 apply_wall_ms=1415 apply_process_cpu_ms=1390 total_wall_ms=4377 total_process_cpu_ms=2633 metadata_api_calls=1 body_batch_api_calls=124 maximum_in_flight_batches=1 maximum_body_bytes=131072 append_calls=2 commit_calls=1 sqlite_append_port_wall_ms=274 sqlite_metadata_port_wall_ms=0 controlled_body_port_wall_ms=2 sqlite_commit_port_wall_ms=48 network_rtt=not_measured
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1285 filtered out; finished in 8.55s


```

## 分析结果

1. `ceil(3968/32)=124`；新旧认证canonical字节数和chunk数一致，最大在飞batch为1，最终各自一次commit。大结果member必须跨多个batch，生产路径完成了每chunk和完整member SHA256核验，否则本测试不会进入commit。
2. metadata API32→1、body API3968→124。按真实remote metadata每API两次read_batch、body batch一次read_batch的实现，远端验证批次数可推导为旧`2×32+3968=4032`、新`2+124=126`。这不是本轮真实Turso transport测量；32块实际单read_batch的adapter证据仍由独立SQL回归提供。
3. **本轮新路径total wall更长（4377ms对2648ms）**。新planner phase wall2961ms、进程CPU1242ms有较大差距，说明wall中存在等待/被调度等CPU外时间，但本实验不能确定成因。加上固定顺序、单样本和commit wall差异（195ms对48ms），不能推导稳定耗时收益，也不能把更长wall直接归因于批量验证。
4. 本轮总CPU新2633ms、旧2345ms；新路径增加完整member SHA256及response-fit检查。没有使用profiler拆出各开销，不能断言这就是全部CPU差值的原因。

## 结论

大正文跨batch的原文认证、调用数公式、128KiB canonical响应上界与单次atomic commit得到受控路径支持。两轮只支持减少串行资源/推导远端批次数，不支持生产延迟、吞吐、整体内存或稳定总CPU收益的结论。

## 异常与发现

本轮无失败尝试。原始输出有一次crate编译，耗时15.33s在路径计时外。采样顺序和系统噪声没有被重复/随机化控制，保留为局限。

两轮完成后，经主agent授权，将单query范围规则从资源层和batch构造器收敛到`ArchiveChunkQuery::validate`，保留完全相同上限及资源InvalidInput/detail映射；该纯规则归位后未重新进行性能采样，最终快照由统一Rust验证覆盖。

## 输入指纹

以下核心路径SHA256与第一轮相同（上述query验证归位在独立types batch文件及resources/archive.rs）：

- `peri-agent/src/agent/compact_v2/micro_batch_test.rs`: `436f9b0369264f688fcbd2b4b2ecbc83646cb9597dd34c2aaf7f4eedc3b66bc7`
- `peri-agent/src/agent/compact_v2/micro.rs`: `6c3099a777f20d3baf6ba95af6eced49606f887d6b1b13d020bcb8cd2dad1328`
- `peri-agent/src/context_archive.rs`: `5fb8d0de884a9b5b950d54bfc180a938efbc6ddff7a41e3ac791aef380f63b5b`
- `peri-resources/src/sessions/remote/session_archive.rs`: `98de37b46ec55f8bb2fd7b4945a4b8ea304968b81dd3372cf70be26825094c1d`

## 下一步建议

请独立验证第二轮原始计数和SHA跨batch行为；综合结论仅保留确定的批次数/边界/原子提交事实。真实Turso网络和多次随机顺序采样留待有实际部署环境时测量。

采样后的最终复核另给仅Unix benchmark使用的 `apply_serial_baseline` 添加 `#[cfg(unix)]`，避免Windows严格检查产生未使用函数告警；Mac测量函数体和执行路径未改变，没有重采样。上列harness SHA保留为采样快照；交付harness SHA为 `1ba39a1ddabf02163dcd7199726f49a52f2ea1a348a20ab7bff886aeac5c82b7`。其他三个测量核心文件指纹保持。
