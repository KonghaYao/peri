# S12 侦察报告：peri-resources 冷开锁 5s 抖动

- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`（HEAD `3ccf75864`）
- 范围声明：**只做侦察，未改动仓库任何源码**。新增的文件全部是未跟踪的探针/日志（`.recon/a1-raw/`、`/tmp/lockprobe/`、`/tmp/lockwatch2/`）。
- 入口命令：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib`

---

## 1. 结论

1. **报错来自哪里**：`lock_schema_open` 的 acquire 超时分支，不是别的失败原因。`connection.rs:283-287` 在「预算耗尽且仍 `WouldBlock`」时返回 `io::ErrorKind::TimedOut` + 固定文案 `session database initialization is busy`。文案在仓库内唯一。
2. **谁持锁**：`<canonical db path>.schema-lock` 上的 `flock(LOCK_EX | LOCK_NB)`（macOS 上 Rust std 的 `try_lock` 映射为 flock，OFD/per-description 语义）。锁从 `open()` 进入时（`connection.rs:124`）持有，覆盖「只读预检 + WAL 连接池 + `init_schema()`（含 `BEGIN IMMEDIATE`）」，直到 `open()` 返回才 drop。
3. **为什么"超 5s"这个表述是误导的**：失败站点全部是 `Duration::ZERO` 预算的探测调用。零预算下不需要任何人持锁 5s，**只要探测瞬间锁被占用哪怕 1ms 就立刻报 TimedOut**。所以真正的问题是"零预算断言对任何瞬时占用零容忍"，而不是"5s 不够长"。
4. **抖动源（观察 + 假设分层）**：
   - 观察 1：同进程内的 flock 是 per-description；`fork()` 会复制 description，父进程 drop 后锁**不释放**，直到继承者退出（自建探针 C 实测延后 2.0014s = 子进程 sleep 时长）。
   - 观察 2：抖动现场（interposer 日志）显示 —— 抖动路径的锁 fd 在 25.5ms 前的 `fork` 中被继承，且抖动时刻 `same_path_inproc=0`（本进程 fd 表里没有该路径的 fd，持有者在本进程之外）。
   - 观察 3：单个测试二进制内含大量自我 spawn（`current_exe()`）用例（`connection_open_test.rs:471`、`context_test.rs:356`、`schema_test.rs:573`、`workspace_probe_test.rs`×4 等），fork 极频繁（单轮 546 次）。
   - 假设（未直接证实到"哪一个子进程"）：某并发测试 fork 时，把**别的测试**正持有的 `.schema-lock` fd 一并复制进子进程；exec 前的窗口或子进程存活期内锁未释放，撞上零预算探测 → 报错。
5. **因果性**：与本分支改动**无因果关系**。`git log` 显示 `connection.rs` / `connection_open_test.rs` 最后修改均早于本分支三个 commit；`git diff 19215d3a1..HEAD` 对这两个文件与 `peri-time/` 输出为空。本分支新增大量 SQLite 并发测试，属**放大既有 flake 的触发概率**，不是引入缺陷。
6. **最小修复**：三处 `Duration::ZERO` 探测改为有限非零预算（与文件内既有 40ms/1s 约定对齐）。属**消除测试自伤**（false failure），不是掩盖产品缺陷。详见 §4。

---

## 2. 证据

### 2.1 代码路径（观察）

`peri-resources/src/sessions/sqlite_store/connection.rs`：

```
:16  const READ_ONLY_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
:17  const SCHEMA_OPEN_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
:124 let _schema_lock = lock_schema_open(&db_path, SCHEMA_OPEN_LOCK_TIMEOUT).await?;
```

```
:242 schema_lock_path()  → canonical db path + ".schema-lock"（先 canonicalize 父目录，软链/别名共享同一 inode）
:259 lock_schema_open(path, budget):
       OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)
       loop { spawn_blocking(|| file.try_lock()) }
       WouldBlock → remaining = deadline - monotonic_now(); if remaining.is_zero() {
                       return Err(std::io::Error::new(ErrorKind::TimedOut,
                                                    "session database initialization is busy"))
                    } else { sleep(remaining.min(10ms)) }
```

锁语义来自 Rust std（`rustc 1.99.0` sysroot `library/std/src/sys/fs/unix.rs:1384`）：`libc::flock(fd, LOCK_EX | LOCK_NB)`，`EWOULDBLOCK` → `TryLockError::WouldBlock`。

失败站点（三个，全部 `Duration::ZERO`）：

| 站点 | 用例 | 代码语义 |
| --- | --- | --- |
| `connection_open_test.rs:377:66` | `incompatible_schema_is_rejected_without_wal_or_database_changes`（循环 `unsupported-{0,12..19}.db`） | 拒绝未知 schema 后，验证初始化锁已被释放 |
| `:310:62` | `failed_migration_closes_connections_and_releases_initialization_lock` | 迁移失败后，验证锁已被释放 |
| `:149:62` | `initialization_lock_has_a_budget_and_preserves_the_sidecar` | `drop(held)` 后，验证锁立即可再获取（随后用 40ms 预算验证"等待者不提前返回"） |

三处共同的断言语义是"**前一个持有者已经释放了锁，现在应该能拿到**"——是辅助性记录断言，不是产品行为验证。

### 2.2 复现（稳定命中，非偶发）

编译产物：`target/debug/deps/peri_resources-5e5b2d6b81e29ee9`（523 个 test）。

单次 cargo 入口（1 次）：

```
test result: ok. 497 passed; 0 failed; 26 ignored; finished in 12.97s
```

循环 20 轮（`--test-threads=18`，脚本 `.recon/a1-raw/loop.sh`，日志 `run-1..20.log`，汇总 `summary-threads18.txt`）：

```
EXP: N=20  RED=16  GREEN=4   每轮 10–30s
失败次数聚合：
  11 × connection_open_test.rs:377:66
   6 × connection_open_test.rs:310:62
   1 × connection_open_test.rs:149:62
```

必红原句（`run-1.log`，stderr 原文）：

```
thread '...incompatible_schema_is_rejected_without_wal_or_database_changes' (105790396)
panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:377:66:
called `Result::unwrap()` on an `Err` value: session database initialization is busy
```

**命中率 ≈ 80%（16/20 轮红）**，远高于此前复核者报告的 6 跑 2 红。

### 2.3 控制组：只跑该模块 → 全绿

`.recon/a1-raw/exp.sh module-only 20 sessions::sqlite_store::connection::open_tests --test-threads=13`：

```
EXP module-only: N=20 RED=0 GREEN=20  args=[sessions::sqlite_store::connection::open_tests --test-threads=13]
```

即：**该模块自身不产生抖动；干扰源在全量套件的其它用例**。这条把嫌疑范围直接缩小到"同进程的其它测试"（自我 spawn / fork）。

产品路径（5s 预算）在本机 20 轮 + cargo 单跑中 **0 次失败**。

### 2.4 根因探针（自建，独立于仓库）

`/tmp/lockprobe/probe.rs`、`probe_d.rs`（编译为 `probe`、`probe_d`）：

| 探针 | 测什么 | 结果 |
| --- | --- | --- |
| A | 同进程第二个 description `try_lock` | `WouldBlock` ⇒ **per-description（flock/OFD）**，非进程级锁；释放后重取 60µs |
| C | 纯 fork（不 exec）子进程继承，父进程 drop 后 | 锁**不释放**，延后到子进程结束（实测 **2.0014s** = 子进程 sleep 时长） |
| B | fork+exec 窗口（`/bin/sleep`） | 仅 **25–85µs**，5 轮 0 次 >1ms ⇒ fork/exec 单独不足以解释长延迟 |
| D1–D4 | 同进程额外未加锁 fd（含 truncate 写 fd、另一线程持有的 fd） | 均**不**导致锁残留 ⇒ 排除"额外 fd 存活导致不释放" |

结论：唯一能造成**毫秒级以上**延迟释放的机制是 **fork 继承 description 且继承者未及时退出/exec**。

### 2.5 抖动现场快照（interposer 观测）

`/tmp/lockwatch2/lockwatch2.c` → `lockwatch2.dylib`（`clang -isysroot "$(xcrun --show-sdk-path)" -dynamiclib`），只插桩 `flock`/`fork`/`posix_spawn`，仅在 `.schema-lock` 上 NB 失败时记录：

`w3-1.log`（`PV_LOCK_FORK=1 PV_LOCK_DEEP=1`，该轮 `494 passed; 3 failed`）：

```
501  266808815817 96830 FORK(fork) child=97826 schema_lock_fds=18
       fd116=/private/var/folders/.../.tmpm3R3e1/unsupported-16.db.schema-lock   ← 该锁 fd 被继承
...
503  266808841290 96830   probe path=.../.tmpm3R3e1/unsupported-16.db.schema-lock
       same_path_inproc=0 scanned                                              ← +25.5ms，NB 失败，持有者不在本进程
```

同一轮三个失败（`w3run-1.log`）：

```
connection_open_test.rs:158:10  Err value: Elapsed(())          ← 1s 等待未拿到锁
connection_open_test.rs:255:6   Err value: Elapsed(())
connection_open_test.rs:377:66  Err value: session database initialization is busy
```

`:158/:255` 是"等 1s 仍拿不到锁"，`:377` 是"零预算瞬间失败"——**两者同时出现**，与"锁被进程外实体（fork 继承者）持续持有 1s 级"的假设一致。且 `:377` 的路径 `unsupported-16.db` 正是该测试循环 `version=16` 那一轮（`connection_open_test.rs:336` 构造 `format!("unsupported-{version}.db")`），现场与失败站点精确对上。

轻量版本跑全量 3 轮：每轮 **1 red / 496 passed**，无测试挂起；`same_path_inproc=0` 在所有抖动中成立。

---

## 3. 因果性判定（git 证据）

```
$ git log -1 --format='%h %ad %s' -- peri-resources/src/sessions/sqlite_store/connection.rs
57d42aeac 2026-10-10 refactor(resources): move tool environments onto independent workspace identity

$ git log -1 --format='%h %ad %s' -- peri-resources/src/sessions/sqlite_store/connection_open_test.rs
ad447afea 2026-10-11 feat(workflow): workflow 编排改造为 builtin MCP 实例 (#181)
```

两者均**早于**本分支三个 commit（`95c1e07d7` / `9b96f456a` / `3ccf75864`）。

```
$ git diff --stat 19215d3a1..HEAD -- .../connection.rs .../connection_open_test.rs peri-time/ | wc -l
0            ← 未触碰

$ git diff --stat 19215d3a1..HEAD -- peri-resources/ | tail -1
 29 files changed, 5637 insertions(+), 140 deletions(-)   ← 集中在历史分页/远端读取
```

**判定：无因果关系**。抖动是既有 flake（历史证据同向：`.recon/S9-verification.round-1.md` 已建议"给该处等待一个小的非零预算，或把只读/锁类测试串行化"；`S6-verification.md` 记录串行 481/481 全绿；`spec/issues/2026-10-12-langfuse-telemetry-spool-report.md:315` 记 CI 亦曾命中 `:377` 并判偶发）。本分支新增的并发 SQLite 测试抬高了 fork 密度与线程竞争，属**概率放大**。

---

## 4. 最小修复方案

### 方案 A（推荐）：三处零预算探测改为有限预算

```diff
--- a/peri-resources/src/sessions/sqlite_store/connection_open_test.rs
@@ -149 (initialization_lock_has_a_budget_and_preserves_the_sidecar)
-    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
+    let held = lock_schema_open(&path, Duration::from_millis(250)).await.unwrap();

@@ -310 (failed_migration_closes_connections_and_releases_initialization_lock)
-    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
+    let held = lock_schema_open(&path, Duration::from_millis(250)).await.unwrap();

@@ -377 (incompatible_schema_is_rejected_without_wal_or_database_changes)
-    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
+    let held = lock_schema_open(&path, Duration::from_millis(250)).await.unwrap();
```

- 为何是最小：不改产品代码、不改锁语义、不改断言语义（"锁最终可获取"仍然被验证，只是不再要求"微秒级瞬时"）。
- 为何不是掩盖：被测缺陷（锁泄漏）若真实存在，会持续到进程退出，250ms 预算照样失败。当前被误报的是"锁在 25ms 级窗口内被 fork 继承者持有"——这是**测试二进制内部的进程耦合**，不是产品语义。生产路径不会 fork 自身。
- 已知代价：对"fork 继承导致锁延迟释放"的检出能力下降（子进程存活 <250ms 时不再报错）。该场景只存在于测试环境。
- 若嫌 250ms 仍不够稳：可直接对齐文件内既有约定用 `Duration::from_secs(1)`（`:161`、`:255` 处已是 1s），代价是失败时多等 1s。

### 候选对比

| 方案 | 做法 | 代价/风险 | 是否掩盖症状 |
| --- | --- | --- | --- |
| **A（推荐）** | 三处 ZERO → 250ms/1s | 极低；不改产品代码 | 否（消除测试自伤） |
| B | 零预算失败后加有限重试 | 小；仍依赖时序 | 部分是 |
| C | 上调 `SCHEMA_OPEN_LOCK_TIMEOUT`（5s→更长） | 对零预算站点**完全无效**（预算为零）；且掩盖真实的 close/迁移阻塞 | **是** |
| D | 锁按 db 路径分桶 | 现状已是"按 canonical 路径一文件一锁"，同路径竞争不受影响；新增锁粒度语义风险 | 不解决本现象 |
| E | 测试内串行化（`--test-threads=1` 或独立二进制） | 有控制组支持（S6 串行全绿），但牺牲并发覆盖，抖动对 CI 仍随机 | **是（规避）** |
| F | 锁与 schema 版本检查解耦（预检改无锁只读） | 语义改造大；须评估"预检期间另一进程迁移"的 TOCTOU | 否，但超出最小修复 |

**若采纳 A，本机预期：16/20 轮的失败通道被切断**（本机 20 轮中产品 5s 路径 0 次失败，模块内全绿，唯一失败通道就是这三处零预算探测）。

---

## 5. 未验证项与局限

1. **未定位到"具体哪个测试的哪个子进程"**导致某一次具体超时；只证实现场（该锁 fd 在 fork 中被继承 + 抖动时 `same_path_inproc=0`）。
2. **未验证 CI（Linux）**是否同样由 fork 继承导致。本机为 macOS，日志路径为 `/private/var/folders/...`（`flock` 语义与 Linux 一致，但 `posix_spawn` 在 Linux/glibc 的实现路径不同）。CI 曾命中 `:377`，属同站点，未做平台级证伪。
3. **loop 只跑满 19/20 轮**时做过一次汇总（后补第 20 轮），结论不受影响；最终统计为 20 轮完整。
4. **未验证三处 ZERO 站点之外的失败形态**：本机 20 轮中产品 5s 预算路径 0 次失败，因此"生产路径是否会抖"在本机无实证（只有 `:158/:255` 的 1s `Elapsed` 在 interposer 干扰下出现）。
5. **未验证候选修复的实际效果**——按任务要求未落代码；§4 结论是静态推理 + 控制组外推。
6. `w3run` 那轮的 3 个失败出现在 interposer 已把 `fork` 拖慢的实验条件下，**不能当作失败率证据**，只作现场快照。
7. 早期一版 interposer 曾令多个测试卡 60s+（已弃用并清理），其 19 次 FLOCK-BUSY 均非 flaky 站点，未纳入结论。

---

VERDICT: MINIMAL-FIX-CLEAR
