# Issue #127：TUI 长时运行内存诊断

**状态**：第五轮长 dose 完成
**日期**：2026-09-07
**范围**：诊断采样与受控实验；不实施性能修复

## H2 假设与当前判定

- **H2a：synthetic workload 能否验证诊断设施对 live nested allocation 的观测机制。** 仅获得 synthetic 机制支持；不能外推真实 TUI 长时行为。
- **H2b：释放 live owner 后是否出现 resident-only high-water，且该高水位是否在 `post_collect` 后仍持续。** 第三轮四个 run 出现 resident-only high-water；严格定义未获得完整支持，因为 `post_collect` 消除了该高水位。
- **H2c：resident 回落后，macOS physical footprint 是否仍显著高于 baseline。** 第三轮不支持；回落后 footprint 仅高于 baseline 0.12–0.37 MiB。
- **Sampler observer effect：高频采样是否造成显著、持续的内存影响或异常波动。** 此分类独立于 H2c；第三轮短时 heavy/light 对照未观察到显著影响。
- **H2d：真实 SQLite/thread scan 或 sqlx pool 导致长期增长。** 本轮受控实验不支持按查询持续增长；观察到可回收的平台化 resident/footprint high-water。

## 设施口径

生产 TUI 仅在受支持平台上设置 `PERI_MEMORY_DIAGNOSTICS=1|true|yes` 且提供安全 basename 形式的 `PERI_MEMORY_DIAGNOSTICS_PATH` 时启动；该变量不再接受目录或完整路径，`experiment` 也不被生产 parser 接受。输出固定在应用私有 `~/.peri/diagnostics/`：既有 `~/.peri` 必须是当前用户所有的非 symlink 目录，允许常见的 0700/0750/0755，只拒绝 group/other 可写；缺失时以 0700 创建。`diagnostics` 子目录必须是当前用户所有的非 symlink 目录且保持 0700。目标文件使用 create-new、0600 与 no-follow，拒绝 separator、`..`、symlink、已有文件和非普通目标。缺 basename、非法 interval/size limit 或 interval 低于 250 ms 时 warning 并 fail-closed，错误不包含实际路径或配置值；Windows 明确 warning 后 fail-closed，且不创建空文件。默认周期 5000 ms，默认 JSONL 上限 64 MiB；`PERI_MEMORY_DIAGNOSTICS_MAX_BYTES` 可在 1 KiB–1 GiB 内覆盖。到达限额时只在完整 JSON line 写入前停止并 warning 一次，不 append 或覆盖既有文件；重复运行须提供新 basename。此方案收窄到应用私有目录，但不宣称抵御能够替换该目录的同用户恶意进程；完整 dirfd/openat TOCTOU hardening 不在本诊断设施威胁模型内。

生产 schema 只记录 aggregate allocator/process/atom counts；phase、expected counts、synthetic bytes/released fraction、scan count/items/checksum 等实验字段仅在 experiment phase 中序列化，避免生产记录出现误导性的 `state_matches_expected_counts=false`。字段安全测试使用显式 JSON key allowlist，不依赖无效的敏感字符串探针。不序列化正文、路径值、session ID、secret 或用户内容。

`VIEW_MODELS` 与 `THREAD_LIST` 分别提取 aggregate 后立即释放 guard，不同时持锁。`sysinfo`、Mach `task_info(TASK_VM_INFO)` 与 mallctl 同步执行；physical footprint 在计算 `sample_duration_us` 前完成，因此采样时长覆盖完整采样。macOS 同时检查 `task_info` 返回成功且 returned count 足够覆盖 `phys_footprint` 字段。历史采样耗时低于 1.4 ms，本轮不引入 `spawn_blocking`，避免制造并发 blocking backlog。

macOS `physical_footprint_bytes` 使用本机 SDK `mach/task_info.h` 的 `TASK_VM_INFO` rev1 prefix，通过最小 `unsafe` 调用 `libc::task_info`；非 macOS 返回 `None`，不以 RSS 冒充 footprint。

生产 shutdown 复用 kit entry 的 `CancellationToken`：cancel 后 sampler 不再开始新 sample，已经开始的 JSON line 会完整写入并 flush；entry 在 `teardown_app` 前通过 helper 以 2 秒 timeout await sampler handle，超时后 abort 并再次 await，正常完成或 `JoinError` 都被收束。cancel 期间输出测试逐行解析 JSON，timeout helper 测试确认挂起 task 最终已结束。

## 第一轮：allocator 推断无效

第一轮使用 `cargo test -p peri-tui --lib`，lib test binary 不继承 `src/main.rs` 的 `#[global_allocator]`。因此第一轮所有 jemalloc 数值均 **invalid for allocator inference**，只保留为设施开发历史：

```jsonl
{"timestamp":"2026-09-07T06:15:31.372991+00:00","elapsed_ms":2,"sample":0,"phase":"startup","rss_bytes":17612800,"jemalloc_allocated_bytes":104448,"jemalloc_active_bytes":147456,"jemalloc_resident_bytes":4259840,"jemalloc_retained_bytes":0,"jemalloc_metadata_bytes":4203544,"jemalloc_mapped_bytes":8536064,"view_models_items":0,"thread_list_items":0,"view_models_shallow_bytes_estimate":0,"thread_list_shallow_and_string_capacity_bytes_estimate":0}
{"timestamp":"2026-09-07T06:15:31.623287+00:00","elapsed_ms":252,"sample":1,"phase":"periodic","rss_bytes":19693568,"jemalloc_allocated_bytes":104448,"jemalloc_active_bytes":147456,"jemalloc_resident_bytes":4259840,"jemalloc_retained_bytes":0,"jemalloc_metadata_bytes":4203544,"jemalloc_mapped_bytes":8536064,"view_models_items":0,"thread_list_items":0,"view_models_shallow_bytes_estimate":0,"thread_list_shallow_and_string_capacity_bytes_estimate":0}
{"timestamp":"2026-09-07T06:15:31.873278+00:00","elapsed_ms":502,"sample":2,"phase":"periodic","rss_bytes":19709952,"jemalloc_allocated_bytes":104448,"jemalloc_active_bytes":147456,"jemalloc_resident_bytes":4259840,"jemalloc_retained_bytes":0,"jemalloc_metadata_bytes":4203544,"jemalloc_mapped_bytes":8536064,"view_models_items":0,"thread_list_items":0,"view_models_shallow_bytes_estimate":0,"thread_list_shallow_and_string_capacity_bytes_estimate":0}
{"timestamp":"2026-09-07T06:15:32.123082+00:00","elapsed_ms":752,"sample":3,"phase":"periodic","rss_bytes":19709952,"jemalloc_allocated_bytes":104448,"jemalloc_active_bytes":147456,"jemalloc_resident_bytes":4259840,"jemalloc_retained_bytes":0,"jemalloc_metadata_bytes":4203544,"jemalloc_mapped_bytes":8536064,"view_models_items":0,"thread_list_items":0,"view_models_shallow_bytes_estimate":0,"thread_list_shallow_and_string_capacity_bytes_estimate":0}
{"timestamp":"2026-09-07T06:15:32.373961+00:00","elapsed_ms":1003,"sample":4,"phase":"periodic","rss_bytes":19742720,"jemalloc_allocated_bytes":104448,"jemalloc_active_bytes":147456,"jemalloc_resident_bytes":4259840,"jemalloc_retained_bytes":0,"jemalloc_metadata_bytes":4203544,"jemalloc_mapped_bytes":8536064,"view_models_items":0,"thread_list_items":0,"view_models_shallow_bytes_estimate":0,"thread_list_shallow_and_string_capacity_bytes_estimate":0}
```

## 第二轮：独立验证失败

第二轮专用 jemalloc binary 的 allocator self-check 有效，且 **H2a 合成 workload 获得支持**。但实验同时保留本地 owner 和 atom 深 clone，把 96 MiB logical workload 放大到约 162 MiB allocated；没有 physical footprint；阶段切换也没有 transition/expected-count 稳定窗口。因此：

- **H2a：支持。** 可确认合成 workload 被 jemalloc 覆盖。
- **H2b：未观察到有效证据/不支持。** 第二轮释放结论不可采信。
- **H2c：未测。** 没有 heavy/light 对照。
- **H2d：未测。** 没有 DB。

第二轮验证结论为 **FAIL**；其原始 JSONL 留在当轮系统临时目录，历史数字不用于第三轮推断。

## 第三轮设计

第三至第五轮的 `peri-memory-diagnostic` 是当时存在于 `peri-tui` 的历史实验 binary；它使用生产同款 jemalloc，并在显式 experiment gate 下执行。最终架构复审发现其直接导入 `peri-resources::sessions::SqliteThreadStore` 违反 TUI layer-imports。由于 `peri-resources` 不能反向依赖 TUI sampler/atom schema，否则形成依赖循环；把混合实验原样迁移还会复制诊断设施。当前树因此删除该 binary 与 feature target，只保留 production sampler 和本 spec 的原始数据、命令及结论历史。

以下历史命令（包括 `PERI_MEMORY_DIAGNOSTICS_PATH`、`--features memory-diagnostic-experiment` 与 `peri-memory-diagnostic`）不再能由当前树执行，仅记录当时实际实验。诊断历史不要求永久保留可重放 binary；若未来需要新 SQLite 实验，应在 `peri-resources` 内独立设计 feature-gated target，且不得依赖 TUI 类型或复制 production sampler。

workload 由 atom 唯一深 owner 持有：32 个真实 `TuiUserBubble/TuiRenderUnit`，各 1 MiB string；32 个真实 `ThreadSummary`，各有两个 1 MiB string，logical requested bytes 为 96 MiB。发布后构造局部立即 drop。半释放从 atom 当前值重建 16/16 个 owner；`im::Vector` 以新 vector 替换，不声称 `shrink_to_fit`；thread `Vec` 显式 `shrink_to_fit`。全释放以空容器替换。

每个阶段先发布状态，再发布 `transition=true` metadata；等待完整 interval + 50 ms 后切为 stable；只统计 `transition=false && state_matches_expected_counts=true` 的样本。稳定窗口 heavy 为 3 秒；light 为 interval + 1 秒，确保每阶段至少一个稳定末值。

### 命令与原始数据

```bash
root="$(mktemp -d)"
for mode in heavy light; do
  if [ "$mode" = heavy ]; then interval=250; else interval=5000; fi
  for run in 1 2 3; do
    PERI_MEMORY_DIAGNOSTICS=experiment \
    PERI_MEMORY_DIAGNOSTICS_PATH="$root/$mode-$run.jsonl" \
    PERI_MEMORY_DIAGNOSTICS_INTERVAL_MS="$interval" \
    cargo run --release -p peri-tui --features memory-diagnostic-experiment --bin peri-memory-diagnostic
  done
done
```

原始 JSONL：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/tmp.X7M2XN32Kc/`。

### 阶段稳定样本摘要

以下为三个 run 的阶段中位数范围，括号内为跨 run 最大 peak；单位 MiB。heavy 每阶段 12 个稳定样本，light 每阶段 1 个稳定末值（`all_released` 为 2 个）。

| mode/phase | RSS median range (peak) | footprint median range (peak) | allocated median range (peak) | resident median range (peak) |
| --- | ---: | ---: | ---: | ---: |
| heavy baseline | 15.42–15.45 (15.47) | 8.42–8.45 (8.45) | 2.64–2.70 (2.70) | 9.77–9.84 (9.84) |
| heavy full | 113.03–113.06 (113.06) | 106.08–106.11 (106.11) | 97.89–97.95 (98.10) | 107.53–107.61 (107.61) |
| heavy half | 64.60–72.50 (72.50) | 57.63–65.55 (65.55) | 49.55–49.89 (49.89) | 59.31–66.89 (66.89) |
| heavy all | 15.88–23.75 (23.75) | 8.87–16.77 (16.77) | 1.48–1.89 (1.89) | 10.56–18.14 (18.14) |
| heavy post_collect | 15.58–15.77 (15.83) | 8.58–8.79 (8.83) | 1.47–1.87 (1.88) | 9.53–10.09 (10.14) |
| light baseline | 15.36–15.39 (15.39) | 8.36–8.41 (8.41) | 2.62–2.70 (2.70) | 9.64–9.75 (9.75) |
| light full | 113.00–113.03 (113.03) | 106.03–106.08 (106.08) | 98.06–98.14 (98.14) | 107.50–107.61 (107.61) |
| light half | 64.38–72.50 (72.50) | 57.41–65.55 (65.55) | 49.87–49.95 (49.95) | 58.88–66.89 (66.89) |
| light all | 15.62–23.76 (23.77) | 8.63–16.78 (16.78) | 1.87–1.95 (1.95) | 10.12–18.14 (18.14) |
| light post_collect | 15.53–15.58 (15.58) | 8.53–8.59 (8.59) | 1.87–1.95 (1.95) | 9.58–9.69 (9.69) |

### 回落与 sampler effect

- full → half：allocated 回落约 48 MiB；footprint 回落约 40.5–48.7 MiB；RSS 回落约 40.5–48.7 MiB。
- full → all：allocated 回落约 96 MiB；两种自然回收轨迹下 footprint/RSS 回落约 89–97 MiB。
- full → post_collect：heavy footprint 回落约 97.3–97.5 MiB，light 回落约 97.5 MiB；RSS 回落约 97.3–97.5 MiB。
- heavy 采样耗时中位数 438–580 µs，peak 1,378 µs；light 中位数/peak 399–704 µs。
- heavy/light 的 full、half、all、post_collect 中位数与 peak 落在相同两类回收轨迹，没有随 250 ms 采样出现单向累积或超出 light 的异常波动。该结果归类为独立的 sampler observer-effect 对照：短时未观察到显著影响，不作为 H2c 的定义或判定依据。

## 第三轮判定与限制

- **H2a：仅 synthetic 机制支持。** 96 MiB atom-owned logical workload 对应约 95.4 MiB allocated 增量，验证了诊断设施能观察 synthetic live nested allocation；不能外推真实 TUI 长时行为。
- **H2b：严格定义未完整支持。** 第三轮四个 run 出现 resident-only high-water，但 `post_collect` 消除了该高水位；因此不能判定为持续的 resident-only retention。
- **H2c：不支持。** resident 回落后，macOS physical footprint 仅高于 baseline 0.12–0.37 MiB，没有观察到显著且持续的 footprint 高水位。
- **Sampler observer effect：短时未观察到显著影响。** heavy/light 对照独立于 H2c；250 ms sampler 未表现出相对 5000 ms sampler 的单向累积或异常波动，但不能证明其他平台或长期负载下绝无影响。
- **H2d：第五轮长 dose 进一步反对线性 per-query live retention。** 15 分钟 replace/250 ms scan 完成 3,600 次 measured queries，allocated 全窗与后 50%/25%/10% slope 均接近零，500-query block median 非单调；严格结论仍不外推为不存在其他 owner 或更长时间尺度的增长。

实验未驱动 terminal render、ACP、Markdown cache、provider 或 network；前三轮未驱动真实 thread scan。短阶段不能证明长时稳定。同步 sysinfo/Mach/mallctl 在其他平台或高负载下仍可能阻塞 runtime。Windows jemalloc 查询仍为 stub。sqlx connection 数仍因具体 store 无无侵入公开入口而缺失。

## 第四轮：H2d SQLite 周期 scan 对照

### 设计

专用 `peri-memory-diagnostic` 增加显式 `PERI_MEMORY_DIAGNOSTIC_WORKLOAD=query-only|replace-projection`，且仍要求 `PERI_MEMORY_DIAGNOSTICS=experiment`。每轮只在系统 tempdir 创建独立 SQLite DB/JSONL；不读取用户 DB，不初始化 provider，不访问 network。固定合成 cwd/title/id，不写 message body：直接创建 4,000 个 `message_count=1` 的 thread row，固定 RFC3339 timestamp；因此 `list_thread_entries(cwd)` 每次返回 4,000 item，但不把 message body 纳入 thread-list 工作集。固定后才启动 sampler、scan task 和 measured phase。

两种 workload 分离 owner：`query-only` 在 checksum 后立即 drop 查询 `Vec`；`replace-projection` 按生产 `service_snapshot` 路径把结果转换为 `ThreadSummary` 并 replace `THREAD_LIST`，不保留历史 `Vec`。三档为 scan-off、2,000 ms、250 ms；off 保持同一 tokio interval task/runtime/sleep 结构，只跳过 DB 调用。每轮 sampler 均为 250 ms，warmup 4 s，steady-state 22 s，release binary；每格 3 run。2 s 档 measured 样本间发生 10–11 次查询，250 ms 档 87–88 次，形成约 8 倍 query-count dose。每份主 DB 文件均为 643,072 bytes；SQLite logical page size 为 700,416 bytes，另有初始化写入产生的 WAL/SHM，均留在对应 tempdir。

JSONL 新增安全 aggregate `scan_count`、`last_result_items`、`result_checksum`；不记录 cwd/path/title/id/content。具体 `SqliteThreadStore` 未提供无侵入 pool statistics，且不值得为一次实验扩大生产 API，因此 pool size/idle 保持 unavailable。

短 pilot：4,000 item、250 ms、replace projection 在 3 s 内完成 13 次 scan，主 DB 当时约 688 KiB，确认查询可测且不会错过 tick。正式原始 JSONL：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/tmp.7tPke7PEAP/`；最终 collect 观察：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/tmp.Oos53ESSxx/`。

### 命令

```bash
cargo build --release -p peri-tui --features memory-diagnostic-experiment --bin peri-memory-diagnostic
root="$(mktemp -d)"
for workload in query-only replace-projection; do
  for mode in off 2000 250; do
    for run in 1 2 3; do
      PERI_MEMORY_DIAGNOSTICS=experiment \
      PERI_MEMORY_DIAGNOSTICS_PATH="$root/$workload-$mode-$run.jsonl" \
      PERI_MEMORY_DIAGNOSTICS_INTERVAL_MS=250 \
      PERI_MEMORY_DIAGNOSTIC_WORKLOAD="$workload" \
      PERI_MEMORY_DIAGNOSTIC_SCAN_INTERVAL_MS="$mode" \
      PERI_MEMORY_DIAGNOSTIC_ENTRIES=4000 \
      PERI_MEMORY_DIAGNOSTIC_WARMUP_SECS=4 \
      PERI_MEMORY_DIAGNOSTIC_MEASURED_SECS=22 \
      ./target/release/peri-memory-diagnostic
    done
  done
done
```

### steady-state 时间序列结果

斜率是对全部 stable samples（每轮 88–89 个）的 OLS，单位 MiB/min；delta 是 measured phase 首末样本差，单位 MiB。括号为三轮范围。`Δ/query` 由首末 delta / 期间 query count 得到；负 allocated 值表示 warmup 后继续回落，不代表释放量可归因于单次查询。

| workload / interval | measured queries | allocated delta / slope | resident delta / slope | RSS delta / slope | footprint delta / slope |
| --- | ---: | ---: | ---: | ---: | ---: |
| query-only / off | 0 | -0.98…-0.66 / -2.37…-1.74 | 0.00 / 0.00 | -0.30…0.08 / 0.06…0.09 | -0.36…0.08 / 0.04…0.08 |
| query-only / 2s | 11 | -2.68…-2.55 / -8.23…-4.84 | 2.11…2.94 / 1.32…8.09 | 3.66…3.97 / 8.52…10.02 | 2.34…3.50 / 5.41…9.44 |
| query-only / 250ms | 87–88 | -0.47…-0.04 / -3.29…-0.01 | 0.23…3.56 / 0.98…4.80 | 1.02…4.19 / 3.23…8.09 | 0.84…2.89 / 3.22…5.69 |
| replace / off | 0 | -0.98…-0.58 / -2.75…-1.60 | -0.05…0.00 / -0.05…0.00 | -0.27…0.06 / 0.10…0.19 | -0.23…0.06 / 0.08…0.19 |
| replace / 2s | 10–11 | -2.82…-1.64 / -8.28…-3.62 | 2.44…3.09 / 4.75…8.99 | 3.41…4.50 / 8.04…12.15 | 3.19…3.42 / 7.02…8.94 |
| replace / 250ms | 88 | -0.71…-0.05 / -0.34…0.05 | 1.20…2.20 / 2.76…5.66 | 2.28…3.20 / 4.64…10.29 | 1.45…2.33 / 2.78…6.84 |

每轮 query 归一化 resident delta：query-only 2s 为 196–273 KiB/query，250ms 为 2.8–41.5 KiB/query；replace 2s 为 250–315 KiB/query，250ms 为 14.0–25.6 KiB/query。footprint 分别为 218–326、9.8–33.6、297–350、16.9–27.1 KiB/query。高 dose 下单位 query 增量显著下降，而绝对 delta 未随约 8 倍 query count 放大；该形态与一次性或阶梯式 high-water 一致，不支持线性 per-query retention，但 owner 尚未归属，不能具体归因为 allocator arena、SQLite page cache 或其他层。

窗口敏感性披露：`replace-projection/250ms/run3` 的 allocated 全窗 slope 为 **+0.052 MiB/min**，后 50% 为 **+0.510 MiB/min**，属于第四轮不可忽略的小正 slope；`replace-projection/2000ms/run3` 后半窗口也出现异常正 slope，但最后 12 个样本趋于平台。第四轮时长太短，局部窗口不足以独立验证平台化，因此结论限于“不支持线性 per-query live retention”，并预注册第五轮长 dose 复核。

### H2d 判定与限制

- **H2d（周期 `list_thread_entries(cwd)` 导致持续增长）：第四轮仅不支持线性 per-query live retention，独立验证结论为 FAIL，待长 dose。** scan 组的 allocated 全窗总体回落，但 `replace/250ms/run3` 有 +0.052 MiB/min 全窗和 +0.510 MiB/min 后半小正 slope，`replace/2000ms/run3` 后半也有异常正 slope、仅最后 12 样本趋于平台；resident/RSS/physical footprint 上升约 0.2–4.5 MiB。250 ms 的约 8 倍 dose 未产生同比绝对增长，形态与一次性或阶梯式 high-water 一致，但 owner 未归属。
- **projection owner 不是持续增长源。** replace 与 query-only 的范围重叠；replace 保留当前 4,000 个 `ThreadSummary`，但每 tick replace 旧 projection，没有历史 `Vec`。
- **可回收性是独立观察。** 额外一次 replace/250ms run 只在最终阶段调用一次 `alloc_collect`：末个 steady sample allocated/resident/RSS/footprint 为 2.932/23.594/31.859/22.813 MiB，collect 后为 2.686/14.969/25.312/16.282 MiB；支持 high-water 大部分可回收，但不参与增长判定。
- 22 s steady-state 只能否定本数据量和短时 query dose 下的持续线性增长，不能证明数小时运行、不同 SQLite/sqlx 版本、其他 OS、并发 writer 或更大 DB 绝无慢增长。各轮 DB 是相同合成内容的独立副本，而非同一 inode；这避免跨轮 pool/page-cache 生命周期污染，但不能消除 OS cache 噪声。
- 未获得 pool size/idle；没有扩大 `ThreadStore` 或 `SqliteThreadStore` 生产 API。没有运行完整 `service_snapshot` 的 Cron/MCP/file scan 等旁变量，replace projection 仅复现 thread scan/projection/atom owner 链。

## 第五轮：15 分钟 replace projection 长 dose

### 本轮采用的分析与判定规则

只运行最低成本的 `replace-projection`：scan-off 与 250 ms 各 1 run；固定 4,000 threads、独立系统 temp DB，warmup 30 s、measured 900 s、sampler 2,000 ms。scan run 结束后单独 `alloc_collect`，off 不 collect。实验时长环境变量保持只在显式 diagnostic binary 生效，并 fail-closed：warmup 允许 1–300 s，measured 允许 1–1,800 s；缺失时仍使用原实验默认 4/22 s，不影响生产 binary。30 秒 warmup 只是固定的阶段边界，不构成收敛验证；scan warmup 尾部 physical metrics 仍有约 0.8 MiB 变化，因此 measured phase 起点可能仍包含 high-water 过渡。

本轮采用以下规则；当前没有独立 Git 记录或外部时间证据能够证明这些规则在查看实验数据前已经冻结：对 allocated/resident/RSS/footprint 计算全窗、后 50%、后 25%、后 10% OLS；scan 每 500 queries 计算 block median；记录最后 10% range/delta、off 同时长背景 drift、query count，collect 前后分开。live retention 主指标为 allocated。“明显超出 off”要求同窗口 scan slope 大于 `max(|off slope|, 0.10 MiB/min)`，且 scan 最后 10% delta 大于 `max(off 最后 10% range, 0.25 MiB)`；两条件同时成立。后 25% 与后 10% 不满足双阈值且 500-query block median 非单调，定义为后窗口平台。resident/RSS/footprint 只作物理 high-water 辅助证据。

执行异常：初次 shell 后台链式启动仅成功启动 off，scan 未启动；发现后在同一 tempdir 单独启动 scan。因此两组参数与时长相同，但不是严格 wall-clock 同期，off 仅作为相邻时段背景 drift。未为增加样本数再运行第二个 15 分钟 scan。

### 命令、路径与真实耗时

```bash
# 两组均使用以下公共参数：
PERI_MEMORY_DIAGNOSTICS=experiment
PERI_MEMORY_DIAGNOSTICS_INTERVAL_MS=2000
PERI_MEMORY_DIAGNOSTIC_WORKLOAD=replace-projection
PERI_MEMORY_DIAGNOSTIC_ENTRIES=4000
PERI_MEMORY_DIAGNOSTIC_WARMUP_SECS=30
PERI_MEMORY_DIAGNOSTIC_MEASURED_SECS=900

# off
PERI_MEMORY_DIAGNOSTIC_SCAN_INTERVAL_MS=off
# scan；仅此组最终 collect
PERI_MEMORY_DIAGNOSTIC_SCAN_INTERVAL_MS=250
PERI_MEMORY_DIAGNOSTIC_FINAL_COLLECT=1
```

原始 JSONL/DB：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/tmp.J8OCO4pP5t/`。每组 measured phase 实际为 900.0 s、451 个稳定样本；加 30 s warmup 后主运行约 930 s，scan 另有约 2 s post-collect。scan measured query count 为 3,600，最后 aggregate scan count 为 3,720，每次 4,000 item。实验结束 checkpoint 后主 DB 文件与 logical page size 均为 700,416 bytes，WAL 为 0；这与第四轮运行中观测到的 643,072-byte 主文件及独立 WAL 状态不矛盾。

### 窗口 OLS、delta 与 range

单位均为 MiB 或 MiB/min；每格为 `slope / delta / range`。

| run/window | allocated | resident | RSS | footprint |
| --- | ---: | ---: | ---: | ---: |
| off full | -0.04335 / -1.0303 / 1.0303 | +0.05883 / +0.6875 / 0.7188 | -0.36220 / -6.0156 / 6.2656 | +0.03147 / +0.3906 / 0.4063 |
| off last 50% | -0.04357 / -0.1864 / 0.5432 | +0.07895 / +0.4688 / 0.5469 | +0.01364 / -0.0469 / 0.4375 | +0.03947 / +0.2344 / 0.2656 |
| off last 25% | -0.06939 / -0.3304 / 0.3304 | -0.01207 / -0.0313 / 0.0313 | -0.12943 / -0.4375 / 0.4375 | -0.00599 / -0.0156 / 0.0156 |
| off last 10% | -0.01892 / -0.0258 / 0.0258 | 0.00000 / 0.0000 / 0.0000 | -0.08393 / -0.1406 / 0.1406 | 0.00000 / 0.0000 / 0.0000 |
| scan full | -0.00029 / +0.0106 / 0.1434 | -0.02061 / -0.1250 / 2.2500 | -0.09942 / +0.4375 / 3.0469 | +0.02279 / +1.2344 / 1.7500 |
| scan last 50% | +0.00159 / +0.0002 / 0.1434 | +0.04284 / +0.2813 / 1.9063 | -0.27229 / -0.4688 / 3.0469 | +0.05216 / +0.3906 / 1.6875 |
| scan last 25% | -0.00302 / -0.0096 / 0.1434 | -0.00470 / +0.2031 / 0.8906 | -0.05463 / -0.5000 / 2.7813 | -0.00479 / +0.3594 / 0.8750 |
| scan last 10% | -0.00353 / -0.0157 / 0.1210 | +0.06522 / -0.0469 / 0.6406 | +0.40946 / +0.8594 / 0.9063 | +0.09159 / +0.0781 / 0.5156 |

scan 的 allocated slope 在所有预注册窗口均远低于 0.10 MiB/min 与对应 off drift 门槛；最后 10% delta 为 -0.0157 MiB，也未超过 `max(0.0258, 0.25)=0.25 MiB`。RSS 后 10% slope 虽为 +0.4095 MiB/min，但 delta 仅 +0.8594 MiB、range 0.9063 MiB，且 allocated 同窗下降；不作为 live retention 证据。

### 每 500-query block median

| query block end | samples | allocated | resident | RSS | footprint |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 500 | 48 | 2.9977 | 23.7656 | 28.7500 | 23.3130 |
| 1000 | 63 | 3.0025 | 23.6719 | 29.1875 | 23.7505 |
| 1500 | 62 | 3.0008 | 22.6563 | 28.4375 | 22.9849 |
| 2000 | 63 | 2.9968 | 22.8906 | 28.5781 | 23.1568 |
| 2500 | 62 | 2.9876 | 23.2031 | 28.9063 | 23.5005 |
| 3000 | 63 | 2.9964 | 23.2344 | 28.6719 | 23.6099 |
| 3500 | 62 | 3.0018 | 23.3281 | 27.2656 | 23.6490 |

allocated block medians 位于 2.9876–3.0025 MiB，顺序上下波动而非单调递增。resident/RSS/footprint block medians也非单调。表格只包含 7 个完整 500-query block，截止 query 3,500；measured phase 最后约 100 queries 构成不完整 block，未列入 block median 表，因此 block 分析不覆盖全部 3,600 次 measured queries。

### collect 与严格判定

scan 最后 steady sample到唯一一个 post-collect 样本：allocated `3.0006 → 2.7917 MiB`，resident `23.2031 → 14.9688 MiB`，RSS `28.1406 → 22.3438 MiB`，footprint `23.5787 → 17.1255 MiB`。post-collect 只有一个采样点，只能作为即时观察值，不能代表 collect 后的稳定分布或稳定水平；可回收性观察不参与 slope 判定。

**严格 H2d 判定：第五轮在该固定工作集和 3,600-query dose 下进一步反对 `list_thread_entries(cwd) → replace projection` 的线性 per-query live retention。** 后 25% 与后 10% allocated 均为微负 slope，未超过本轮采用的 off/数值门槛；完整 500-query block median 非单调。物理指标仍存在约 MiB 级波动/阶梯式 high-water，owner 未归属；30 秒 warmup 未验证收敛，最后约 100 queries 未纳入 block median，且由于仅各 1 run、off 与 scan 非严格同期、判定规则无独立事前冻结证据、未观测 pool counters、未覆盖并发 writer 和完整 service snapshot，本轮不能证明不存在其他 owner、非线性增长或更长时间尺度问题。
