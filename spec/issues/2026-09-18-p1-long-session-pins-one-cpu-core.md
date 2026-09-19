# P1：长会话下单个 peri 进程持续占满一个 CPU 核（热点在 tokio worker 的内存拷贝）

**状态**：Open
**优先级**：P1（用户指定）
**类型**：性能 / 上下文压缩 / 长会话
**创建日期**：2026-09-18
**来源**：本机运行时事故；用户报告“有个进程 100% CPU 占用”，并明确指出“不是 python 的”
**最后核查**：2026-09-18 22:18（Asia/Shanghai）—— 第二轮实测：线程级 CPU 差分、两次独立 `sample`、日志/事件/DB/WAL 增量实测、execution-lock 反查
**相关 issue**：
- `2026-09-10-p0-full-micro-compact-churn.md`（**机制同源**：Full `estimated_tokens_saved` 恒 0、compact 反复触发；状态 Fixed 待现场验收。本 issue 的“反复 Full / Started 无 Completed”结论必须与它对齐，见“与既有 issue 的区别”一节）
- `2026-09-04-tui-long-markdown-streaming-cpu.md`（TUI 超长 Markdown 流式输出的 CPU 增长；状态 In verification，未覆盖本现象）
- `2026-08-18-cpu-max-damage.md`（多进程 SQLite 轮询风暴）—— 采样特征不同，见同一节

## 问题描述

在同一用户同时运行多个 `peri` TUI 的情况下，其中一个进程（PID 19346，工作目录 `remote-control-server-ui-instance-fix`）长时间占用约一个 CPU 核：90 秒连续观测无任何空闲间隙，稳定在 ~108%。用户侧观感即为“CPU 一直是 100%”。

**2026-09-18 22:0x–22:1x 的第二轮实测推翻了本 issue 首版的两条判断：**

- 该进程并非“仍在正常工作”。在累计 **110 秒的自旋窗口（14:12:36–14:14:59Z）内 CPU 稳定 103.8–107.9%，而日志新增 0 字节 / 0 行、全部事件计数为 0、`threads.db` 与 `-wal` 增量为 0**——烧满一个核但零可观测产出，判定为**空闲自旋**（见“空闲自旋判定”一节）。首版所依据的“数据库持续写入 / 模型 API 连接不断重建”中的连接数部分来自测量错误（`lsof -p <pid> -i` 在 macOS 是 **OR 语义**，需加 `-a`；实测仅 1–2 条 ESTABLISHED，非 183 条）。
- 会话归属更正（见“现场数据”一节）：其 execution-lock（`sha256(thread_id)` 反查，方法已在 PID 93903 上验证）指向 **`01a0b3a5-3dae-70d2-8ca5-08588d9bc2a9`**；首版记录的 `01a0b397-3524-…` 只是启动后 7 秒被载入过（同一 bridge 的 reset 计数器 1→2→3 为证），并已于 13:50:00Z 随其 TUI 退出关闭。

同时它不是恒定满载，而是**间歇性风暴**：观测期内出现约 6 分钟（14:05:58–14:11:57Z）的 ~2–3% 低负载平台，随后恢复自旋；`ps` 的衰减均值会把这种模式显示成“一直 100%”。

用户期望：长会话不应该让单个 TUI 进程持续吃满一个核。（补充期望：TUI 已退出的残留进程不应继续占核并占住终端。）

## 现场数据

### 进程与 CPU

| 项目 | 值 |
| --- | --- |
| 进程 | `peri`，PID 19346，二进制 `~/.peri/agent-v3.17.0/peri`（`peri --version` = `3.17.0`，与仓库 `agent-v3.17.0` 标签同源） |
| 启动时间 | 2026-09-18 16:32:25（本地）= 08:32:25Z |
| 工作目录 | `/Users/konghayao/code/pazhou/remote-control-server-ui-instance-fix` |
| 环境 | macOS 26.5.1 (25F80)，arm64 |
| 90 秒窗口（21:5x） | 累积 CPU 每秒 +1.08s，无空闲间隙（≈108%） |
| 28 分钟窗口（21:5x） | CPU 由 9:43 → 30:24，即 28 分钟墙钟消耗 20.7 分钟 CPU（≈74%） |
| 10 分钟窗口（22:04:52–22:14:59，本轮） | 累计 CPU 51:10.12 → 55:56.86（+4:46），其中含约 6 分钟低负载平台 |
| 自旋窗口（22:12:36–22:14:59，累计 110 秒） | `pcpu` 逐次 107.9 / 108.5 / 105.8 / 104.2 / 103.4 / 104.9 / 103.9 / 103.4 —— **稳定贴顶、无空闲间隙** |
| 线程分布（`ps -M` 差分，本轮） | 单条 tokio worker UTIME 19:05.80 → 22:04.89（+2:59），末态 `%CPU 98.7 / STAT=R`；主线程 3.9%、次线程 0.2%，其余 <0.5% |
| 进程状态（22:1x） | TUI 已于 13:50:04Z 退出，但进程未退出，仍是 `ttys004` 前台进程组（`TPGID == PGID == 19346`，该终端被占住）；`~/.peri/metrics/2026-09-18.jsonl` 自 13:45Z 起停止增长 |
| 同期其他 peri 进程 | 54272 / 78838 均低于 1%，只有 19346 被钉住 |

### 会话与数据规模

| 项目 | 值 |
| --- | --- |
| 活跃会话（execution-lock `d0d5cc2f…`，`sha256` 反查并独立复核） | `01a0b3a5-3dae-70d2-8ca5-08588d9bc2a9`；最后活动 13:58:58Z（`payloads=578 compact=true ok=true`） |
| 已关闭会话（同一进程内，08:32:32Z 载入） | `01a0b397-3524-77a2-bb91-51aa81b636ac`（标题“调整命令/技能面板样式”）；**13:50:00Z 被 Ctrl+C 关闭**，之后无任何事件；其 execution-lock `20e7087b…` 当前无人持有 |
| 已关闭会话的规模 | 2064 条消息，内容合计约 5,527 KB；`excluded=1` 2015 条、`truncated=0`、`projection` 全为空 |
| 关闭时刻的日志 | `Adopting canonical transcript snapshot session_id="01a0b397" payloads=2064 compact=true ok=false` → `13:50:04.028Z ACP host terminal shutdown complete` |
| 共享数据库 | `~/.peri/threads/threads.db` 2,495,610,880 B（22:12 实测），`-wal` 在 6 次测量中恒为 5,154,152 B |
| 日志 | `~/.peri/logs/agent-tui.2026-09-18`（多进程共同写入，无 session/pid 字段；自旋窗口内 0 增量） |

### 采样热点

首轮（21:5x，`sample 19346 5`）Top functions：

```text
340  _platform_memmove  (in libsystem_platform.dylib)
139  peri + 0x7c404
 67  peri + 0x599ee0
 67  peri + 0x7c3f0
 63  peri + 0x5d5434
...
```

第二轮做了**两次独立采样**（22:01 与 22:04，5s / 4s），热点线程、主干偏移与叶子分布**几乎逐位重合**，说明这是稳定的循环路径而非随负载波动的成本。主干（偏移相对 load address `0x100e50000`；进程已 strip，只有裸偏移）：

```text
tokio worker 入口 0x8ec638 → … → 0x91c5d0
  → 0x66be1c                       ← task poll 入口
    → 0x5caf88 → 0x5cf824
      → 0x5d2dc4   （2454/4128 样本，占该线程 59%）
         ├─ 0x5d9b80 (1883) → 0x5d8c2c (1441) → 0x5db120 (1438)
         │     ├─ 0x5d4170 → _platform_memmove   (163)
         │     ├─ 0x5db98c → _platform_memmove   (159)
         │     ├─ 0x5db81c → _platform_memmove   (154)
         │     └─ 0x5db9e4 → _platform_memmove   (151)
         ├─ 0x5d8b68 → 0x5d41e8 → 0x5d4574 → 0x5d3ef8
         └─ 0x5d9b44 → 0x5d9404 → 0x5da95c → 0x5daa84 → 0x5d3ef8 → 0x5d5434
      → 0x5d2df4 (1428) → 0x5d87cc (1155) → …
```

两次采样一致的 Top-of-stack 叶子：`_platform_memmove`（305/259）、`peri+0x7c404`（123/114）、`peri+0x7c3f0`（102/110）、`peri+0x599ee0`、`peri+0x5d5434`、`peri+0x5d3ef8`、`DYLD-STUB$$memcpy`、`pthread_getspecific`。

- 全部热点位于 `tokio-rt-worker` 线程内的**内存拷贝路径**（`memmove`/`memcpy`），且**同一工作集群内有 4 个以上不同的直接 `memmove` 调用点**——这种形态符合泛型 `Vec`/`String` 克隆、`extend_from_slice`、JSON 序列化等被单态化内联的拷贝代码。
- 主线程（`DispatchQueue_1: com.apple.main-thread`）绝大多数样本停在 `pthread_cond_wait`。
- `sqlx-sqlite-worker-19/20` 线程停在 `dispatch_semaphore_wait` → `semaphore_wait_trap`（**空闲**），**未见 `pread`/页读取热点**；SQLite 轮询假设可排除。
- 读数注意：macOS `sample` 的线程树计数**包含阻塞线程**（每线程都记满 4128 个样本），因此不能只看计数判断哪条线程在烧 CPU，必须与 `ps -M` 的线程级 CPU 时间差分配合（本轮两者一致指向同一线程）。

### 压缩遥测（同一份共享日志）

```text
CompactStarted     597 次
CompactCompleted    38 次
Compact 完成（agent 侧）  52 次
```

最近 20 分钟（13:30–13:49Z）内：启动 31 次、完成 1 次。完成样本（UTC）：

```text
12:44:55  step=379 strategy=Full affected=212 before=212 after_visible=5
12:55:37  step=477 strategy=Full affected=221 before=221 after_visible=6
12:58:28  step= 86 strategy=Full affected=192 before=192 after_visible=4
13:06:09  step= 74 strategy=Full affected=239 before=239 after_visible=5
13:26:24  step=208 strategy=Full affected=307 before=307 after_visible=6
13:28:06  step=164 strategy=Full affected=193 before=193 after_visible=2
13:45:27  step=305 strategy=Full affected=216 before=216 after_visible=6
```

对应 bridge 侧事件全部为：

```text
CompactCompleted summary_len=20507..23391 trigger=auto strategy=full
                 affected_count=193..307 estimated_tokens_saved=0
```

即：每次 Full Compact 都把 192–307 条可见消息压成 2–6 条，但仍以 `auto` 触发**反复执行**（约每 3–17 分钟一次，跨越 12:44–13:45Z）。`estimated_tokens_saved=0` 不是异常值——`peri-agent/src/agent/compact_v2/full.rs:84,175` 对 Full 结果硬编码为 0（`compact_v2/mod.rs:590` 的 `FullFailed` 路径同样为 0）。

**第二轮全天统计**（22:1x 重数）：bridge 侧 `CompactStarted` **651** / `CompactCompleted` **40**，agent 侧 `Compact 完成` 同为 **40**。即约 **94%（611/651）的 Started 没有配对的 Completed**，且按小时分布（09 时 180/13、10 时 119/5、11 时 43/3、12 时 115/7、13 时 155/8、14 时 36/1）说明这是**全天持续模式**，不是某一时段爆发的偶发。

**配对缺口的代码解释（已核实，取代“559 次空转”的读法）**：结束事件只在 `did_compact` 为真时发射——

```rust
// peri-agent/src/agent/stages/compact.rs:383-385
// `MessagesCompacted` 仅表示确有 Compact mutation；未应用 outcome
// 仅保留 `CompactStarted` 作为 begin 观测，完整 outcome transport 留待后续切片。
if did_compact { ... }
```

因此 `Skipped`（含 `plan_micro` 空 plan 的 no-op）、`Shadowed`、`FullFailed`、连续失败降级**都只有 Started、没有结束事件**；差额 ≈“尝试过但未产生变更”的次数，不能读作真实空转次数。cancel 分支自 S1.4/G6 修复后反而是成对的（`compact.rs:231-243` 发 `CompactEnded{Interrupted}`）。此外 bridge 侧 40 与 agent 侧 40 一致，而首轮记录的 38/52 差异提示 observe 广播还可能有滞后丢事件（`event_v2/bus.rs` 为 broadcast）——**未测到**，需埋点。

**自旋窗口内没有任何 compact 事件**，而每次 compact 尝试都会发 `CompactStarted`——所以**自旋主体不是 compact 循环本身**（见下一节）。

旁证：14:05:35.86–36.91Z 出现 **33 次 `CompactStarted` 挤在 1.05 秒内（~28ms 间隔、0 次 Completed）**，同时 TUI 侧 `committed_len` 由 442 升至 573，紧接着 CPU 从 105% 阶跃至 1.3%。这 1 秒内 33 次尝试与“~28ms 迭代周期”吻合，但**归属进程与因果方向均未证实**（日志无 pid/session 字段，也可能是其他 peri 进程）。

快照采纳记录（`peri-acp/src/host/prompt.rs:630`，同一会话）：

```text
08:19:55  payloads=4     compact=false ok=false
09:03:44  payloads=156   compact=false ok=true
12:57:25  payloads=1347  compact=true  ok=false
13:50:00  payloads=2064  compact=true  ok=false
```

## 空闲自旋判定（2026-09-18 22:12–22:15 实测）

| 观测项 | 自旋窗口（14:12:36–14:14:59Z，累计 110 秒） | 低负载平台（14:08:16–14:09:18Z） |
| --- | --- | --- |
| CPU | **103.8–107.9%**（逐次采样稳定贴顶，无空闲间隙） | 3.3%（1.6 → 1.7%） |
| 单线程 | 自旋线程 UTIME +2:59 / 10 分钟，`STAT=R` | — |
| 日志新增 | **0 字节 / 0 行** | 152 字节 / 1 行（一条 Bash 工具失败 WARN） |
| 事件计数 | `CompactStarted/Completed`、`TextChunk`、`LlmRequest`、`MessagesCompacted`、`Subagent*`、`TurnSuspended`、`TurnDone` **全为 0** | 同上各项均为 0 |
| `threads.db` 增量 | **0 B**（40 秒窗口） | +1,126,400 B |
| `threads.db-wal` | 恒 5,154,152 B（全部 6 次测量不变） | 恒 5,154,152 B |
| TCP 连接 | 1–2 条 ESTABLISHED | 同量级 |

结论：**空闲自旋**（间歇性）。判据是“满核 CPU + 零可观测产出”这一组合，而不是“CPU 高”本身。

需要更正的测量错误：首版的“183 条 ESTABLISHED / 连接不断重建”来自 `lsof -p <pid> -i`（macOS 上是 **OR 语义**，必须加 `-a`）。同一远端 `211.97.92.89:443` 的连接确实在被反复重建（本地端口 52612 → 54633 → 54803 → 55193 每次不同），但同一时刻只有 1–2 条，**不存在连接风暴或 fd 泄漏**；远端身份未测到（反向解析 SERVFAIL）。

同时段内 `threads.db` 的增长（~1.1–1.3 MB/min）出现在 19346 **低负载**的窗口，而在确认自旋的窗口内增量为 0——写入者很可能是其他 peri 进程（**推断**，日志无 pid 字段，无法归属）。

## 符号化归因（已完成）

发布链已确认：`agent-v3.17.0` 标签指向 `fcc3f6fd`（= 当时的 HEAD），由 `.github/workflows/release-agent.yml` 执行 `cargo build -p peri-tui --release --target aarch64-apple-darwin`（profile：`opt-level="z"`、`lto=true`、`codegen-units=1`、`strip="symbols"`），版本号由 tag 注入（工作区的 `0.2.0` 与产物版本无关）。

方法：用 `git archive HEAD` 在 `/tmp` 复现同源构建（`CARGO_PROFILE_RELEASE_STRIP=false`，526,622 个符号）后与发布二进制对齐。**不能按偏移直接取符号**——两者 `__text` size 差 0.48%（发布 `0xba4a64` vs 本地 `0xbb3098`），根因是链接器/SDK 不同（发布 SDK 26.5，本机 15.1），函数大小多重集重合 98.7%。因此改用「函数体字节唯一命中 + 函数大小序列」对齐，并逐项做落点校验。

关键映射（偏移为相对 `__TEXT` 基址的裸值；均在两次独立采样中重复出现）：

| 采样偏移 | 函数（demangle 后） | 证据强度 |
| --- | --- | --- |
| `0x66be1c` | `tokio::runtime::task::raw::poll::<peri_tui::kit::acp_bridge::spawn_acp_bridge_inner::{closure#0}, Arc<Handle>>` | **弱**（仅由 714 个函数大小序列块定位，内容匹配未通过） |
| `0x5caf88` | `peri_tui::kit::acp_bridge::spawn_acp_bridge_inner::{closure#0}` | 强 |
| `0x5cf824` | `peri_tui::kit::acp_events::dispatch_for_bridge` | 强 |
| **`0x5d2dc4`** | **`peri_tui::kit::acp_events::render::push_view_models`**（2454/4128 样本，编译后 ~7.1 KB；`render.rs:25`） | 强 |
| `0x5d9b80 / 0x5d9b44` | `<im::vector::Vector<TuiRenderUnit>>::remove` | 强 |
| `0x5d8c2c` / `0x5d87cc` | `<im::vector::Vector<TuiRenderUnit>>::append` / `::insert` | 强 |
| `0x5d9404` | `<im::vector::Vector<TuiRenderUnit>>::split_off` | 强 |
| `0x5db120` | `<im::nodes::rrb::Node<TuiRenderUnit>>::merge` | 强 |
| `0x5dba10 / 0x5db98c / 0x5db81c / 0x5db9e4` | `<im::nodes::rrb::Node<TuiRenderUnit>>::merge_rebalance`（4 个直接调用 `memmove` 的点在此函数内） | 强 |
| `0x5da95c / 0x5daa84` | `<im::nodes::rrb::Node<TuiRenderUnit>>::split` | 强 |
| `0x5d4170 / 0x5d3ef8 / 0x5d5434` | `<Arc<Chunk<TuiRenderUnit>>>::new` / `::make_mut` / `<Chunk<TuiRenderUnit> as Clone>::clone` | 强 |
| `0x7c404 / 0x7c3f0 / 0x7c41c` | `<alloc::string::String as Clone>::clone` | 强 |
| `0x599ee0 / 0x599f40` | `<TuiRenderUnit as Clone>::clone` | 强 |
| `0x59a16c` / `0x59a23c…0x59a490` | `<TuiAssistantBubble as Clone>::clone` / `<TuiToolCard as Clone>::clone` | 强 |
| `0x5483f8 / 0x548564` | `drop_glue::<[TuiRenderUnit]>` / `drop_glue::<TuiToolCard>` | 强 |
| `0xa3xxxx` 簇、`0xa5b1fc` | **未定位**（jemalloc C 区：本地 clang 与 CI Apple clang 不同，且该区 96% 为 machine outliner 产物） | 失败，不给名字 |

对齐产物：`/tmp/mapping_corrected.txt`（54 行映射表）、`/tmp/rel_starts_raw.txt`、`/tmp/loc_starts_raw.txt`、`/tmp/build-317.log`；构建树 `/tmp/peri-align-317`（全程未修改工作区）。

## 因果链（第二轮修订）

**首版链条中“`mem::take` 整份 transcript 拷贝 → memmove 热点”已被代码证伪**：`std::mem::take` 是 O(1) 的所有权转移（`compact.rs:131-134`），不复制任何字节；`CompactStarted=597/38` 的差额也已由遥测配对缺陷解释（见上节）。此外自旋窗口内没有任何 compact 事件，而 compact 尝试**必然**发 `CompactStarted`——**自旋不是 compact 循环造成的**。

第二轮已确认的事实：

```text
满核自旋（单条 tokio worker，STAT=R，105–107%）
  × 自旋窗口内零日志 / 零事件 / 零 DB 写入 / 1–2 条 TCP 连接
  × 热点主干：acp_bridge 任务 → dispatch_for_bridge → push_view_models（2454/4128 样本）
      其下是 im::Vector<TuiRenderUnit> 的 RRB 树操作（merge_rebalance / split / split_off / insert / append / remove）
      + TuiRenderUnit / TuiToolCard / TuiAssistantBubble / String 的深拷贝 + jemalloc 分配热点
  × 两次独立采样指纹一致（同一线程、同一主干）
  → 结论：CPU 烧在 TUI 侧的「事件 → 视图模型全量重建」路径，与 agent / compact / SQLite 路径无关
```

与工作量的关系：

- 该路径单次成本与 `BridgeState.committed` 长度成正比（会话关闭前 `committed_len` 已达 573），且每轮深拷贝大量渲染单元与字符串——这解释了“会话越长越贵”，与用户“长会话吃满一个核”的观感一致。
- 但零产出自旋还需要一个**持续触发重建的事件源**：自旋窗口内无外部事件、无 I/O、无日志，因此高度怀疑存在**自反馈循环**（重建 → 状态变更 → 再次 dispatch → 再重建）或某个恒就绪的 `poll_change`（采样中另有 `InstantiatedComponent::poll_change`、`UseAtomImpl<Option<FocusedEntry>>::poll_change`）。**未验证**。
- 自旋的触发/退出条件目前只有一次时间吻合的观察：14:05:36.9Z 的 33× `CompactStarted` 爆发结束时 CPU 从 105% 阶跃到 1.3%。**方向未定**（该爆发也可能属于其他进程）。

证据限制：

- `0x66be1c`（任务入口）的符号为**弱证据**（仅大小序列，未通过内容匹配）；`0xa3xxxx` jemalloc 簇未定位；其余主干帧为字节级对齐。
- 共享日志无 session/pid 字段，事件归属只能按时间窗口推断；自旋期日志为 0，**无法从日志判断它在处理哪个会话**。
- “自反馈循环”为**推断**，尚未验证。

## 与既有 issue 的区别

`2026-08-18-cpu-max-damage.md` 的机制是「多进程每 2 秒 `list_threads()` 全表投影 → SQLite 页读取竞争」，采样特征为 **`sqlx-sqlite-worker` + `pread` 热点**。本次观测到的是：

- 热点在 `tokio-rt-worker` 的 `memmove`，`sqlx-sqlite-worker` 全部空闲；
- 只有 1 个进程被钉住，而非多进程轮流升高；
- 与压缩/长会话强相关，与 2 秒轮询周期无对应关系。

因此这是**独立问题**，不能用 2026-08-18 的修复覆盖。

**与 `2026-09-10-p0-full-micro-compact-churn.md` 的关系（首版漏引，必须对齐）**：该 issue 已记录同一批代码事实——Full `estimated_tokens_saved` 恒 0（当时引 `full.rs:81/171`，现为 84/175）、`affected_count` 口径、仅成功 Full 才 reset tracker、A5“同一工作单元最多两次未恢复的 Full”上限；状态 **Fixed（2026-09-11，待现场验收）**，修复提交 `0d98cdc6 / 36ddc645 / c415e661 / 2aa587d3` 均在其后落地。运行中的 `agent-v3.17.0` 二进制 mtime 为 09-18 14:17，**晚于**这些修复，因此本 issue 观测到的反复 Full 更可能是“压缩后上下文真的又长回阈值”的合法路径，而非旧 churn 复现。本 issue 相对它的增量是：**TUI 侧的零产出自旋**与**遥测配对缺口**——两者都不在它的覆盖范围内。

**与 `2026-09-04-tui-long-markdown-streaming-cpu.md` 的关系**：该 issue 处理“流式输出长 Markdown 时对增长中全文的重复处理（Θ(L²/C)）”，属**有产出**的 CPU 升高；本现象是**零产出**空转。但两者同在 TUI 侧、同样表现为“按事件重复处理整份数据”，本轮符号化结果（`push_view_models` 每事件全量重建视图模型）与该方向同源，修复时应合并考虑。

## 复现条件

- **复现频率**：本样本中长时间必现（进程已运行 ≥5.5 小时；TUI 于 13:50:04Z 退出后仍自旋 ≥20 分钟；观测期内另有约 6 分钟低负载平台）。
- **触发步骤（待验证）**：
  1. 让一个 TUI 会话累积到数百个渲染单元（本例会话关闭前 `committed_len=573`，消息 2064 条 / 5.5 MB 量级）；
  2. 关闭该会话的 TUI（本例为 Ctrl+C：`13:50:04.028Z ACP host terminal shutdown complete`），**让进程残留**；
  3. 观察残留进程：CPU 是否在无人操作时仍贴满一个核，而日志 / DB / WAL / 连接**全部零增长**（本例的判定特征）。
- **判定特征**：`ps -M` 显示单条 `tokio-rt-worker` 的 UTIME 持续增长；`sample` 主干落在 `push_view_models`；`lsof -nP -a -p <pid> -i` 只有 1–2 条连接。
- **环境**：macOS 26.5.1 arm64，`agent-v3.17.0`（tag → `fcc3f6fd`），共享 `threads.db` 2.31 GiB，同时多个 TUI 实例。

## 建议的验证方式 / 下一步

**本轮已完成**：线程级 CPU 差分定位到单条 tokio worker；两次独立采样确认热点指纹可复现；日志 / 事件 / DB / WAL 增量实测判定为**空闲自旋**；execution-lock 反查更正会话归属；`lsof` 用法更正；符号化归因完成（热点 = `push_view_models`）。

1. ~~**验证 TUI 侧归因（优先级最高）**：在 `peri-tui/src/kit/acp_events/render.rs` 的 `push_view_models` 上做定向测量~~ —— **第四轮已完成**：`im::Vector` 操作与渲染单元克隆已按阶段定量（见「修复实施」），确认为「每事件全量重建 + COW 深拷贝」并已改造为增量复用。**仍未完成的是事件源判定**：无法从既有证据区分「外部事件持续到达」与「状态变更自反馈」，需按新的第 1 条埋点计数验证。
2. **复现与止损**：复现目标改为“**TUI 退出后残留进程仍在自旋**”（比“长会话”更可能稳定复现）；对残留进程执行终止（本 issue 的 PID 19346 已按此处理）。
3. **判定自旋归属**：在自旋窗口内用带符号构建采样；若不可行，退化为在 `push_view_models` 入口插桩计数（每 10 秒打印调用次数与 `committed.len()`），可直接区分“事件驱动的高频重建”与“单次超长调用”。
4. **遥测配对（独立小修）**：`compact.rs:383-385` 对未应用 outcome 不发结束事件，导致 Started / Completed 长期 94% 不配对，无法据此判断 compact 活性；应补 `CompactEnded{outcome}` 或把 Started 语义改为 attempted，并记录 `attempt_id / transcript 条目数 / 耗时 / 结束原因`。
5. **消除已验证的纯浪费拷贝（次要，与本次自旋无因果关系）**：`agent_context.rs:36-52`（每个 middleware hook 无条件深拷贝整份可见消息）、`transcript.rs:336-350`（`visible_snapshot()` 逐条深拷贝，而 `legacy.rs:194-199` 明确该载荷在本链路无消费者）、`model_bridge.rs:375-383`（每次 LLM 调用无条件深拷贝整棵 provider JSON body）。
6. **不要基于已证伪的假设改动**：首版的 `mem::take` 拷贝假设已证伪；`DISABLE_AUTO_COMPACT=1` 的 A/B 优先级下调（自旋窗口内无 compact 事件），仅在排查 14:05 那 33 次爆发时使用。

## 涉及文件

- `peri-agent/src/agent/stages/compact.rs`（453 行）—— Compact 阶段入口：Skip 判定与 `CompactStarted` 发射（85-149）、`mem::take` transcript（131-134）、`select!` cancel 分支与作废路径（176-255）、失败降级计数。
- `peri-agent/src/agent/compact_v2/mod.rs` —— `determine_compact_action` 与阈值（0.75 / 0.95）、Micro 满足回收目标的分支、`run_full_or_degrade`（569）。
- `peri-agent/src/agent/compact_v2/full.rs` —— Full 结果构造，`estimated_tokens_saved` 硬编码为 0（84、175）。
- `peri-agent/src/agent/compact_v2/planner.rs` —— 收益估算 `before_chars - after_chars / 4`（355）与空 plan 路径（325）。
- `peri-agent/src/agent/token.rs:89` —— `estimated_context_tokens()`：以上一次 LLM usage 的 `input_tokens` 加待计入工具 token 作为压力来源。
- `peri-acp/src/host/prompt.rs:612-644` —— `finish_prompt_turn` 采纳 canonical transcript snapshot（日志见 630 行）。

**第二轮新增（TUI 侧热点，符号化确认，本次自旋的直接热点）**：

- `peri-tui/src/kit/acp_events/render.rs:25` —— **`push_view_models`：本轮自旋热点函数**（2454/4128 样本），每次事件重建 `BridgeState.committed`；源码 25–110 行的克隆经 LTO 内联后形成 ~7.1 KB 函数。**第四轮已改造**（分组增量化 + `join_into` + `mem::take`）。
- `peri-tui/src/kit/acp_events/mod.rs` —— `dispatch_for_bridge`（`0x5cf824`），事件进入渲染重建的入口。
- `peri-tui/src/kit/acp_bridge.rs` —— `spawn_acp_bridge_inner`（`0x5caf88`）及其任务 poll（`0x66be1c`，**弱证据**）。
- 编译后的热点簇：`im::vector::Vector<TuiRenderUnit>`（remove / append / insert / split_off）、`im::nodes::rrb::Node<TuiRenderUnit>`（merge / merge_rebalance / split）、`Arc<Chunk<TuiRenderUnit>>::{new, make_mut}`、`Chunk<TuiRenderUnit>::clone`、`TuiRenderUnit / TuiToolCard / TuiAssistantBubble / String::clone`、`drop_glue<[TuiRenderUnit]>`。

**第二轮新增（经代码核实的每步整份深拷贝；抬高工作期成本，但与本次自旋无因果关系）**：

- `peri-agent/src/agent/agent_context.rs:36-52` —— `from_stage()` 无条件深拷贝整份 `visible_messages`；`peri-agent/src/agent/stages/middleware_runner.rs` 的 11 个 hook 入口均经此构造（每 ReAct 迭代 6–8 次，另加每次工具调用 1 次）。
- `peri-agent/src/session/transcript.rs:336-350` —— `visible_snapshot()` 逐条深拷贝（注释自认），而 `peri-acp/src/session/event_sink/legacy.rs:194-199` 对 `TurnCommitted` 直接返回 `None`，注释写明该载荷“在本链路无消费者……序列化该载荷是纯浪费”。
- `peri-agent/src/agent/model_bridge.rs:375-383` —— 每次 LLM 调用无条件 `prepared.body().as_value().clone()`（整棵 provider JSON body）。

**符号化对齐产物（`/tmp` 临时文件，非仓库文件）**：`/tmp/mapping_corrected.txt`（54 行映射表）、`/tmp/rel_starts_raw.txt`、`/tmp/loc_starts_raw.txt`、`/tmp/build-317.log`、构建树 `/tmp/peri-align-317`。

## 修复实施（第四轮，2026-09-19）

针对第三轮定位的热点函数 `peri_tui::kit::acp_events::render::push_view_models` 实施改造，**仅改分组与快照组装的数据搬运方式，不改视觉行为**。

### 改了什么

| 位置 | 旧 | 新 |
| --- | --- | --- |
| `group_successful_tools` 缓存判定 | 整段折成一个 `u64` 指纹（`group_input_fingerprint` 全文哈希），任何一处变化 → 全段重建 | 逐条身份比较定位首个差异下标（`same_entry` / `first_divergence`），再取「≤ 差异点的最大**稳定切点**」（`GroupCut` + `CutAnchor`）复用前缀 |
| 就地把改写 | 逆序 `segment.remove(i)` + `segment.insert(...)` 在共享 `im::Vector` 上就地改写 → 每次 remove/insert 触发 COW 路径复制（整 chunk 深拷贝） | 只做 `push_back` / `append`，不改写共享节点；深拷贝仅限真正进入折叠组的卡片 |
| 尾部新内容 | 整段指纹失效 → 全量重建（成本 ∝ 段长） | 复用前缀（`split_off(out_cut)` 与缓存共享节点）+ 只重建变化后缀（成本 ∝ 变化量） |
| 快照组装 | `items.append(current_turn.view_models().clone())` | [`join_into`]：按尾长选 `push_back` 或树合并（见下「im 原语实测」） |
| 段拆分 | `items.split_off(0)` | `std::mem::take`（生产唯一取值 `start == 0`） |

### im::Vector 原语实测（release，`TuiRenderUnit` = 320 B，本机 macOS 26.5.1 arm64）

改造依据是三条实测曲线，而不是「O(log n)」的文档描述：

| 操作 | base=1000 时实测 |
| --- | --- |
| `Vector::clone` | 0.01 µs（确为 O(1)） |
| `append` 接 1 个元素 | **157 µs**；接 256 个元素 186 µs —— 退化为整树重建，成本 ∝ `self.len()` |
| `push_back` 循环推 1 个元素 | **5.2 µs**；256 个元素 64 µs |
| `split_off(k)` | k=500：15.6 µs；k≈段尾：4.9 µs —— 走 `Node::split`，按元素 memmove + 固定开销 |
| `Focus::narrow(k..)` 只读窗口 | 扫 1000 条 4.2 µs、扫尾 8 条 0.04 µs（零拷贝视图） |

即：采样中出现的 `Node::merge` / `merge_rebalance` / `Node::split` / `Chunk::clone` 热点正是 `append` 与 `remove`/`insert` 的实现路径，`push_view_models` 每次调用都在付这笔钱。

### 等价性证据

1. **旧实现 vs 新实现逐条等价**：把 HEAD 的 `group_successful_tools` 逐字迁移进测试（`acp_events_test/legacy_group_probe_test.rs`），同一演化序列上**每一步**断言新旧输出 `PartialEq` 全字段相等（200 / 500 / 1000 单元 × 稳态与尾部追加两种序列），全部通过。
2. **增量复用 vs 全量重建**：`acp_events_test/group_incremental_test.rs` 对同一串事件跑两条路径（热缓存增量 / 每次清空缓存强制重建），断言每步 `VIEW_MODELS` 快照逐条结构相等，4 个用例覆盖工具生命周期、流式文本、error 卡片、焦点与折叠覆盖、TurnSuspended 归档（段缩短）。
3. 既有 1615 个 lib 测试全部通过，`cargo fmt --check` 与 `cargo clippy --workspace --all-targets -- -D warnings` 干净。

### 成本对照（release，µs/次调用；A/B 为同源同数据，含每步共享 `clone()` 以复现生产引用计数）

| 序列 | N=200 | N=500 | N=1000 |
| --- | --- | --- | --- |
| A 稳态重复推送（旧 → 新） | 20.2 → **0.3**（60×） | 37.9 → **0.5**（73×） | 79.6 → **1.1**（75×） |
| B 尾部追加（旧 → 新） | 31.6 → 34.9（**0.9×**） | 105.0 → **73.9**（1.4×） | 303.8 → **143.2**（2.1×） |

按阶段拆解（`push_view_models` 内部的 `stage_*_ns` 计数器，N=1000，文本 chunk 事件）：

| 阶段 | 改造前 | 改造后 |
| --- | --- | --- |
| 组装（assemble） | 129.5 µs | **3.2 µs** |
| 折叠 pass | 7.0 | 4.5 |
| 分组（group） | 155.6 | **68.3** |
| 写快照 | 42.4 | 27.0 |
| 合计 | 336.6 | **103.5**（3.3×） |

**必须如实记录的两点**：

- **N≈300 以下尾部追加路径出现轻微回退**（N=200 时 34.9 vs 31.6 µs）。原因是新实现多付两次 `split_off` 的固定开销（各 5–10 µs），换来的是成本不再随段长线性增长。生产现场 `committed_len` 已达 573 且会话继续增长，交叉点（约 N≈300）以下并无实际场景；若未来出现「短会话高频推送」的新特征，可考虑在段长低于阈值时直接全量重建。
- **自旋的触发源仍未定位**。本次修复消除的是被采样钉住的热点函数（2454/4128 样本）的单次成本，稳态重复推送路径已降到 1 µs 量级；但「谁在自旋窗口内持续投递事件」这一环没有新证据。若自旋是自反馈循环，修复后每次迭代变便宜约 75 倍，**不等于循环停止**——需按「下一步」第 3 条继续验证调用频次。

### 本轮新增文件

- `peri-tui/src/kit/acp_events_test/group_incremental_test.rs` —— 差分级等价测试（4 用例，常规运行）。
- `peri-tui/src/kit/acp_events_test/perf_probe_test.rs` —— `push_view_models` 端到端定向测量（`#[ignore]`，手动运行）。
- `peri-tui/src/kit/acp_events_test/append_probe_test.rs` —— `im::Vector` 原语成本曲线（`#[ignore]`，手动运行）。
- `peri-tui/src/kit/acp_events_test/legacy_group_probe_test.rs` —— 旧实现等价性与成本对照（`#[ignore]`，手动运行）。
- `peri-tui/src/kit/acp_bridge.rs` —— 测试专用 `PerfCounters`：分组复用/重建、重建条目数、深拷贝条目数、折叠写回数，以及 `push_view_models` 五个阶段累计纳秒。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-18 | — | Open | agent | 按用户要求登记 P1；基于本机运行时观测与本仓库源码交叉核对，归因部分标注为待验证 |
| 2026-09-18（第二轮） | Open | Open | agent | 更正会话归属（活跃租约为 `01a0b3a5`，`01a0b397` 已于 13:50:00Z 关闭）与“仍在正常工作”判断；实测判定为**空闲自旋**；更正 `lsof` 连接数测量错误；补齐 `2026-09-10` / `2026-09-04` 交叉引用 |
| 2026-09-18（第三轮） | Open | Open | agent | 符号化归因完成：热点为 `peri_tui::kit::acp_events::render::push_view_models`（TUI 视图模型全量重建），推翻首版“compact / transcript 拷贝”假设；同步更新复现条件与修复方向 |
| 2026-09-19（第四轮） | Open | Open | agent | 实施 `push_view_models` 分组增量化与快照组装改造；新旧实现逐条等价、差分测试通过；给出 im 原语实测曲线与 A/B 成本对照。**自旋触发源仍未定位，现场复现验证待做** |

## 修复记录

### 已实施（第四轮）

- **`push_view_models` 热点改造**：分组由「整段指纹 + 全量 COW 重建」改为「逐条差异定位 + 稳定切点前缀复用 + 只重建变化后缀」；快照组装按尾长选择 `push_back`/树合并；段拆分改 `std::mem::take`。等价性与成本见上节。涉及 `peri-tui/src/kit/acp_events/render.rs`、`peri-tui/src/kit/acp_bridge.rs`（仅测试埋点）。
- **止损**：PID 19346 于符号化取证完成后终止（其 TUI 已于 13:50:04Z 退出、自旋期零产出、无连接风暴）。

### 待修复（按优先级）

1. **自旋触发源**（本轮未解决，仍是本 issue 的核心未知）：确认自旋窗口内的事件来源是外部持续投递，还是「重建 → 状态变更 → 再次 dispatch」的自反馈。既有证据指向事件驱动（采样链 `dispatch_for_bridge → push_view_models`），但自旋期日志 / DB / WAL / 事件计数全为 0，无法从日志侧判定。建议在 `push_view_models` 入口埋点计数（每 10 秒打印调用次数与 `committed.len()`），区分「事件驱动的高频重建」与「单次超长调用」。
2. **现场复现验证**：修复后需在「长会话 + TUI 退出后残留」的原始场景下复测，确认残留进程的 CPU 是否回落到 0——注意第四轮的回退提示（N≈300 以下尾部追加略慢），复测时同时记录 `committed_len`。
3. compact 观测配对缺口（`compact.rs:383-385`）：未应用 outcome 不发结束事件，Started / Completed 长期 94% 不配对——补 `CompactEnded{outcome}` 或改 Started 语义为 attempted。
4. 三处已验证的纯浪费整份深拷贝：`agent_context.rs:36-52`、`transcript.rs:336-350`、`model_bridge.rs:375-383`。
5. 若第 2 条复测显示尾部路径仍是瓶颈：把快照元素改为 `Arc<TuiRenderUnit>`（或让缓存保存「已按切点预拆的前缀」），消除复用前缀 `join_into` 时的右脊 COW 复制。当前 `group` 阶段残值（N=1000 时 68 µs）主要来自这处。

- 首版的 `mem::take` 拷贝假设与 `DISABLE_AUTO_COMPACT` A/B 不再是主线索（见“因果链”与“下一步”）。
