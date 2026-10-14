# S12 冷开锁抖动：最小修复与验证（B1）

- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`（HEAD `3ccf75864`，本任务**未提交**）
- 依据：`.recon/S12-cold-open-lock.md`（侦察结论 `MINIMAL-FIX-CLEAR`，方案 A）
- 入口命令：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib`
- 本次改动文件（唯一）：`peri-resources/src/sessions/sqlite_store/connection_open_test.rs`（`git diff --numstat` = +132/−10，629 行，未超 `STD-SIZE-001` 1000 行上限）
- 未改动：`connection.rs`（产品锁路径）、DDL、public 契约、其他 crate。
  工作树里 `peri-tui/src/kit/acp_events/history_page.rs` 的 1 行改动**不是本任务所为**，全程未触碰。

---

## 1. 结论

1. **改了什么**：把同文件内 4 处「零容忍锁获取」改为有限预算 `LOCK_RELEASE_PROBE_BUDGET = 250ms`，并给 2 个子进程 helper 的锁获取加上同预算的有界重试；新增 1 条确定性证伪测试。
   - 侦察报告 §4 点名的三处（`:159` / `:322` / `:391`，报告行号 `:149`/`:310`/`:377`）→ 预算化。
   - **报告未点名但同类、且被实测命中**的第四处（`:450`，symlink 别名用例的第二次获取）→ 预算化。
   - 两个子进程 helper（`:492`、`:555`）的 `try_lock().unwrap()` → `acquire_initialization_lock_within(..., 250ms)`。
2. **只按报告的三处改是不够的**：改完后 20 轮里**原三处 0 命中**，但同类通道转移到 `:452`（2 次）与 `:505`（1 次）→ 3/20 轮红。这正是「修一类问题，而不是一个问题」的证据（见 §2.2）。
3. **修的是根因还是症状**：修的是**测试自身的假失败通道**（断言层），不是产品缺陷，也不是掩盖。
   - 机制层根因（`fork` 继承 open file description，锁在本进程之外继续存活）**没有消除**，也不该由这个任务消除（消除它要么串行化测试、要么改锁原语，属规避或大改）。
   - 被修掉的真实缺陷是**断言语义写错**：这些探测的语义是「前一个持有者已释放，锁应当能再拿到」，却写成「锁必须**瞬时**可得」（`Duration::ZERO`）。零预算不是「太短的超时」，是量纲错误——**任何把 5s 调大的做法对它完全无效**（这也是报告排除方案 C 的理由）。
   - 检出能力仍在：真实泄漏会持续到持有进程退出（远长于 250ms），250ms 后依旧报同一个 `session database initialization is busy`。
4. **证伪**：把 `LOCK_RELEASE_PROBE_BUDGET` 临时改回 `Duration::ZERO`（= 退回修复），新增测试 **4/4 次必红**；还原后（终稿代码）同一命令 **5/5 次绿**（§2.4）。
5. **验证规模**：修复后 `--lib` 全量 **30 轮 → 29 轮全绿、锁类 panic 0 次**（唯一 1 轮红是既有的、与本修复无关的 `Git discovery timed out`，见 §4.1）。受影响用例单跑 5/5 绿。

---

## 2. 证据

### 2.1 修复前：同一入口命令即可复现（基线）

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
test result: FAILED. 495 passed; 2 failed; 26 ignored; 0 measured; 0 filtered out; finished in 13.51s
```
5 轮循环（`.recon/b1-raw/loop-cargo.sh 5 .recon/b1-raw/pre-fix`，汇总 `.recon/b1-raw/pre-fix-summary.txt`）：

```
run=01 exit=0    ok. 497 passed; 0 failed
run=02 exit=101  FAILED. 495 passed; 2 failed
   1  panicked at .../connection_open_test.rs:310:62
   1  panicked at .../connection_open_test.rs:377:66
run=03 exit=0    ok. 497 passed
run=04 exit=0    ok. 497 passed
run=05 exit=0    ok. 497 passed
```
`run-2.log` 原文（`.recon/b1-raw/pre-fix/run-2.log:533`）：

```
thread '...failed_migration_closes_connections_and_releases_initialization_lock' (106694051)
  panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:310:62:
called `Result::unwrap()` on an `Err` value: session database initialization is busy
```
基线合计：**6 次入口运行 2 次红**（33%），失败站点与侦察报告 §2.2 完全一致（`:310`、`:377`）。
（首次交互运行只截了尾部、未落盘；上面 5 轮有完整日志。侦察阶段另有独立统计：直接跑测试二进制 `--test-threads=18`，20 轮 16 红，命中站点同为 `:149`/`:310`/`:377`。）

### 2.2 只改报告点名的三处 → 锁类通道转移（3/20 红）

20 轮循环（`.recon/b1-raw/post-fix-summary.txt`、日志 `.recon/b1-raw/post-fix/run-*.log`）：

```
run=01 ok / run=02 FAILED / run=03..06 ok / run=07 FAILED / run=08..19 ok / run=20 FAILED
run=02  1 panicked at .../connection_open_test.rs:452:10  → session database initialization is busy
run=07  1 panicked at .../connection_open_test.rs:505:6   → Elapsed(())
run=20  1 panicked at .../connection_open_test.rs:452:10  → session database initialization is busy
```
- `:452` 是 `canonical_database_and_symlink_aliases_share_one_lock_inode` 里 `lock_schema_open(&existing_alias, Duration::ZERO)` 的 `.unwrap()`。该用例在此之前已经**持有过同一锁**（`:436` 获取并 drop、`:441-446` 的 `tokio::join!` 两个 store 打开各自持有 ms 级窗口），`fork` 继承者会让锁在 drop 之后继续存活 → 零预算探测正好撞上。**同一类缺陷，只是报告没列出这个站点。**
- `:505` 是 `exited_process_releases_initialization_lock_for_retry` 等待子进程 ready 文件的 5s 外层超时（`Elapsed(())`）。
- 原三处（`:159`/`:322`/`:391`）在这一轮 20 次里 **0 命中**。

### 2.3 本次改动（文件、位置、原因）

`peri-resources/src/sessions/sqlite_store/connection_open_test.rs`：

| 位置（终稿行号） | 改动 | 原因 |
| --- | --- | --- |
| `:14` | 新增 `const LOCK_RELEASE_PROBE_BUDGET: Duration = Duration::from_millis(250)` | 单一权威：释放探测/获取探测的等待预算，附「为什么不能是零」的机制注释 |
| `:159` | `initialization_lock_has_a_budget_and_preserves_the_sidecar` 的释放探测 | 报告 §4 站点 1（实测 1 次命中，见报告 §2.2 聚合 1×`:149`） |
| `:322` | `failed_migration_closes_connections_and_releases_initialization_lock` 的释放探测 | 报告 §4 站点 2（基线实测命中） |
| `:391` | `incompatible_schema_is_rejected_without_wal_or_database_changes` 的释放探测 | 报告 §4 站点 3（基线实测命中，最高频） |
| `:450` | `canonical_database_and_symlink_aliases_share_one_lock_inode` 的别名获取 | 同类缺陷，报告未列但 3 站点修复后实测 2/20 轮命中 |
| `:462` | 新增同步 helper `acquire_initialization_lock_within(path, budget)` | 子进程侧同类的零容忍获取 |
| `:492` / `:555` | 两个子进程 helper 改用上面的函数（原 `file.try_lock().unwrap()`） | `:505` 观测到的可疑通道（子进程拿不到锁 → 永不写 ready） |
| `:576` | 新增测试 `lock_release_probe_budget_tolerates_an_external_holder` + `:549` 的 `initialization_lock_holder_child_process` | 确定性证伪（§2.4） |

**预算取值的理由**：250ms 与同文件既有的 `READ_ONLY_BUSY_TIMEOUT`（`connection.rs:16`）一致；覆盖侦察报告 interposer 实测的 25.5ms 继承窗口有 ~10× 余量；比对齐 `Duration::from_secs(1)` 失败时少等 750ms。仅覆盖测试断言等待，不进入产品路径。

**刻意保持 `Duration::ZERO` 的位置（不是漏改）**：

| 位置 | 为什么安全 |
| --- | --- |
| `:253` | 轮询循环，`Err(TimedOut)` 是正常分支（等被测线程拿到锁即 break） |
| `:436` / `:506` | 该锁文件在本用例里的**首次**获取（全新 tempdir 路径），此前不可能存在持有者 |
| `:439` / `:453` / `:609` | **否定**断言（`is_err()` / `unwrap_err()`），要求的就是「拿不到」 |

### 2.4 证伪：退回修复 → 测试必红（临时改动 → 红 → 还原）

临时把常量改回零（`sed -i '' 's/Duration::from_millis(250)/Duration::ZERO/' ...`，仅改常量一行，调用点不动）：

```
$ grep -n "LOCK_RELEASE_PROBE_BUDGET: Duration" peri-resources/src/sessions/sqlite_store/connection_open_test.rs
14:const LOCK_RELEASE_PROBE_BUDGET: Duration = Duration::ZERO; // TEMP-FALSIFY

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- sessions::sqlite_store::connection::open_tests::lock_release_probe_budget_tolerates_an_external_holder --exact
test ...lock_release_probe_budget_tolerates_an_external_holder ... FAILED
thread '...lock_release_probe_budget_tolerates_an_external_holder' (107446447) panicked at
  peri-resources/src/sessions/sqlite_store/connection_open_test.rs:620:13:
release probe budget did not outlast the holder: session database initialization is busy
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.02s
```
（原句日志：`.recon/b1-raw/falsify-final.log`；扩展前的早一版同类日志：`.recon/b1-raw/falsify-budget-zero.log`。退回状态下 **4 次运行 4 次红**，耗时均 0.01–0.02s，是确定性红而非抖动。）

还原（改回 250ms，无残留标记）后同一命令（终稿代码）**5 次运行 5 次绿**：

```
$ grep -c "TEMP-FALSIFY" .../connection_open_test.rs   → 0
test ...lock_release_probe_budget_tolerates_an_external_holder ... ok
test result: ok. 1 passed; 0 failed; ... finished in 0.02s   （×5）
```

**这条测试的断言链**（`:576`）：先由本进程建立侧车并释放 → spawn 子进程（`current_exe()`，沿用文件内既有 helper 约定）持锁并写 ready → 断言 **零预算探测必失败**（复刻抖动现场：持有者在进程之外，`WouldBlock`）→ 写释放文件 → 断言 **250ms 预算探测能等到持有者退出**。子进程释放时机由**释放文件**驱动，不依赖 sleep 时长，因此两个方向都确定。

### 2.5 修复后：全量循环

```
.recon/b1-raw/final-summary.txt（20 轮，入口命令与要求一致）
run=01..10 ok. 499 passed; 0 failed; 26 ignored   (11.9–15.9s)
run=11 FAILED. 492 passed; 7 failed               ← 与本修复无关，见 §4.1
run=12..20 ok. 499 passed; 0 failed; 26 ignored
```
- 20 轮里 `session database initialization is busy` 出现次数：**0**（`grep -l` 全部 20 份日志为空）。
- `run=11` 的 7 例全在 `discovery_test.rs`，全部是 `workspace discovery failed: Git discovery timed out`（`peri-process` 的 git 子进程 5s 超时），与本修复无调用关系。
- 补跑 10 轮（`.recon/b1-raw/final-extra-summary.txt`）：**10/10 绿**（`ok. 499 passed; 0 failed`，10.7–17s），`session database initialization is busy` 出现 0 次。
- 合计（终稿代码）：**30 轮 = 29 绿 + 1 红（discovery 超时）**，锁类通道命中 **0/30**。
- 交付态复跑一次（`.recon/b1-raw/delivered-final.log`）：`test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 12.83s`（含新增用例 `lock_release_probe_budget_tolerates_an_external_holder ... ok`）。
- 受影响的三个用例单跑各 5 次，5/5 绿：

```
### lock_release_probe_budget_tolerates_an_external_holder        run=1..5  ok (0.02–0.08s)
### canonical_database_and_symlink_aliases_share_one_lock_inode  run=1..5  ok
### exited_process_releases_initialization_lock_for_retry        run=1..5  ok (0.07–0.12s)
```

### 2.6 其他校验

```
$ rustfmt --edition 2021 --check peri-resources/src/sessions/sqlite_store/connection_open_test.rs   → 无输出
$ wc -l peri-resources/src/sessions/sqlite_store/connection_open_test.rs                            → 629
$ git diff --stat
 .../sessions/sqlite_store/connection_open_test.rs | 142 +++++++++++++++++++--
 peri-tui/src/kit/acp_events/history_page.rs       |   2 +-     ← 本任务之前就存在的改动，未触碰
```

---

## 3. 为什么这是「修根因」而不是「把超时调大」

1. 失败站点是 `lock_schema_open(path, budget)` 在 `budget = 0` 时的**立即**返回路径（`connection.rs:280-288`）：零预算下不需要任何人持锁 5s，探测瞬间被占 1ms 就报 TimedOut。因此**放大 `SCHEMA_OPEN_LOCK_TIMEOUT`（5s）对失败站点毫无作用**——报告方案 C 已被排除，本修复也没有动它。
2. 被改动的不是「等待时长」，而是**断言语义**：从「锁必须瞬时可得」改成「锁应在有限预算内可得」。前者断言的是测试进程的调度运气，后者断言的是被测语义（前一个持有者已释放）。
3. 异常路径没有消失：锁真泄漏时，同样的站点、同样的 `session database initialization is busy` 仍会失败，只是从「瞬时」放宽到 250ms。代价可接受且明确：**sub-250ms 的泄漏检出灵敏度下降**（只存在于测试环境，因为泄漏若真实存在会持续到进程退出）。
4. 机制层（`fork` 继承锁 fd）**未消除**，属已知残留（§4.2）。消除它需要测试隔离/串行化（报告方案 E，判为规避）或改锁原语（方案 F，超最小修复），本任务不做。

---

## 4. 未验证项与局限 / 残留风险

### 4.1 与本修复无关的既有 flake（本次观测到 1 轮）
- `run=11` 的 7 例 `Git discovery timed out`；该现象**在本任务之前的日志里已存在**：`.recon/a1-raw/w2run-2.log:532`（侦察阶段原始日志）、`.recon/S6-verification.md:34`、`.recon/R4b-verification.md:42` 均有同族记录，且都伴随「本机另有编译/高负载」。因此判为**既有的负载相关子进程超时**，不属本任务范围，未处理。
- 影响：所以本次 30 轮不是字面上的全绿，而是 **29/30 全绿 + 锁类 0/30 命中**。若验收要求字面上的全绿，需要在机器空闲时重跑（该 flake 在空闲时不出；本次它在 30 轮中只出现 1 轮）。

### 4.2 机制层未消除（已知残留）
- `fork` 继承 `.schema-lock` 的 open file description 依旧是事实：任何**毫秒级以上**的继承窗口仍可能让别的用例的锁在 hold 之外存活。1s/5s 预算的等待（侦察报告行号 `:158`/`:255`，本任务未改动）在 interposer 实验下曾出现 `Elapsed(())`，本修复不覆盖那一类（本机 30 轮未命中）。
- 本次新增 1 条会 spawn 子进程的用例 → 每轮 fork 窗口 +1（既有同构用例 546 次/轮量级），理论上对既有 flake 有 <1% 量级的放大；30 轮内未观察到影响。

### 4.3 未定位项
- `:505` 只观测到 1 次，**未定位**是「子进程锁获取被继承者挡住」还是「子进程启动 >5s」。我按前者的类做了有界重试（覆盖该可能），**没有**顺势把 5s 就绪等待调大（无证据就不加时）。若再现，需仪器化区分二者（记录子进程各阶段时间戳）再决定。
- 未验证 CI（Linux）：本机 macOS/18 核，`flock` 语义一致但 `posix_spawn` 路径不同；CI 曾命中 `:377`（同站点）。

### 4.4 统计强度
- 30 轮全绿（29/30）不足以证明概率为 0（若真实残留率 5%，30 轮里 0 命中的概率仍有约 21%）。真正的判据是：**机制解释 + 可复现的确定性证伪测试 + 原失败站点在 30 轮内 0 命中**（而同一入口在修复前 6 次里红 2 次，3 站点版本 20 轮里红 3 次）。
- 统计样本量小，未做正式显著性检验。

### 4.5 边界确认
- 无 DDL 变更、无表结构变更、无 public 契约变更、无 `Cargo.toml`/依赖变更、无产品代码变更（`connection.rs` 未改）。
- 未 commit（按任务要求，仅改工作树文件）。

---

VERDICT: LOCK-PROBE-CLASS-FIXED-WITH-DETERMINISTIC-FALSIFICATION
