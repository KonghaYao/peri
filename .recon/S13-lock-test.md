# S13 A1：冷开锁回归测试的确定性（B1 修订）

- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`；本任务**未提交**
- 依据：`.recon/S12-verification.md` §4.1–§4.3、§7.2 第 1 条
- 入口命令：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib [-- <filter>]`
- 改动文件（唯一）：`peri-resources/src/sessions/sqlite_store/connection_open_test.rs`，**662 行**（≤ `STD-SIZE-001` 1000 行上限，无需拆文件），`git diff --numstat` = +165/−10
- 未触碰：`connection.rs`（产品锁路径）、DDL、public 契约、其他 crate；未执行 git add/commit/checkout/stash/rebase
- 原始日志：`.recon/s13-raw/`（本轮全部运行、汇总与 sha256）

---

## 1. 结论

1. **确定顺序已用显式握手建立，不是时长猜测。** 持有者在**另一个进程**里取得 `.schema-lock` 后写 `holder.ready`；契约是「读到 `holder.release` 才释放」。父进程读到 `holder.ready` 才发起探测，而 `holder.release` 在**被测调用返回之后**才写——因此「零预算探测」与「共享入口整段调用（含首次 `WouldBlock`）」都确定落在持锁窗口内，并由调用后的第三次零预算探测再次证实。
2. **回归用例护住的是修复的唯一实现处，而不是一个常量。** 四处真实调用点不再各自携带预算字面量，统一走 `probe_initialization_lock_release`（预算只有这一个来源）；新用例走**同一个函数**。把该入口退回原调用点的零等待写法后，用例 **3/3 红**（§4.2，原始输出 + 字节级还原）。
3. **判别断言是「等待不得短于预算」。** 持锁期间重试循环只能以「取得锁」或「越过 deadline」退出，因此修复态下等待时长不可能短于预算（实测 **252.794 ms**）；退回零等待时实测 **0.484 ms**（§4.3）。两个方向都是被测代码的退出条件推出的，不是 sleep 出来的。
4. **边界如实记录（§4.4）**：250 ms 是**容忍窗口**，不是机制修复。本机实测：外部持锁窗口 100 ms → 绿；350 ms → 红。边界落在 (100 ms, 350 ms]，与预算量级一致（未逐毫秒细化）。
5. **残余边界（§4.7，实测）**：把四处调用点**绕过**共享入口、各自内联回零等待时，本用例仍是绿的。守卫覆盖的是共享入口本身；调用点级绕过属于本次未覆盖的写法，已在局限中列出。
6. **验证规模**：新用例单跑 **6/6 绿**；`-p peri-resources --lib` 全量 **3/3 绿，499 passed / 0 failed / 26 ignored**（与基线一致）。

| 要求 | 判定 | 依据 |
|---|---|---|
| 1 显式握手建立确定顺序 | PASS | §3 握手协议；三次探测的持锁状态由因果链推出，非时长猜测 |
| 2 回归用例护住真实调用点（共享 helper 路线） | PASS | §4.2 退回零等待 → 3/3 红，原始输出 + sha256 还原 |
| 2 回归用例护住四处调用点自身（绕过入口） | 未覆盖（实测绿） | §4.7 原始输出 |
| 3 边界与根因/症状如实记录 | PASS | §4.3 实测等待、§4.4 100 ms 绿 / 350 ms 红；未使用「必红/彻底消除」类表述 |
| 4 文件规模 ≤ 1000 行 | PASS | 662 行 |
| 5 连续 ≥5 轮 + 全量一轮 | PASS | §4.1 6/6；§4.5 全量 3 轮 499/26 |

---

## 2. 改动内容

1. 新增共享入口（预算的唯一来源），四处真实调用点与新用例都用它：

```rust
/// 释放探测的统一入口：各用例 `drop` 掉自己的持有者后，用它确认锁已归还（同进程之外
/// 可能还有继承来的持有者，见 `LOCK_RELEASE_PROBE_BUDGET`）。
///
/// 四个真实调用点与回归用例 `release_probe_waits_out_an_external_holder` 共用这一处，
/// 等待预算只有这一个来源——它被退回零等待，回归用例即红。
async fn probe_initialization_lock_release(path: &Path) -> Result<std::fs::File> {
    lock_schema_open(path, LOCK_RELEASE_PROBE_BUDGET).await
}
```

2. 四处调用点改为 `probe_initialization_lock_release(&path).await.unwrap()`（`initialization_lock_has_a_budget_...`、`failed_migration_...`、`incompatible_schema_...`、`canonical_database_and_symlink_aliases_...`）。原先各处内联的预算字面量随之消失。
3. 原用例 `lock_release_probe_budget_tolerates_an_external_holder` 重写为 `release_probe_waits_out_an_external_holder`（1:1 替换，全量用例数不变），并新增两个局部 helper：`assert_initialization_lock_busy`（断言 `TimedOut`）与 `PROBE_WAIT_MEASUREMENT_SLACK = 50ms`（只用于把等待断言写成下界）。
4. 子进程侧持有者 `initialization_lock_holder_child_process` 与其获取 helper 未改（仍为「读到放行文件才释放」+ 有界获取）。

---

## 3. 确定顺序（为什么每一步可判定）

```text
child:  取锁 → 写 holder.ready → 轮询 holder.release（5ms） → drop(file) → 退出
parent: 等 holder.ready → [2] 零预算探测(TimedOut) → [3] 共享入口调用(TimedOut, 等待≈预算)
        → [4] 零预算探测(TimedOut) → 写 holder.release → [5] 共享入口调用(Ok) → child.wait()
```

1. `holder.ready` 只在**取得锁之后**写出，父进程读到它之后的探测必定遇到持锁。
2. 步骤 [2] 是「受控首次 `WouldBlock`」：此刻放行文件不存在，持有者不会释放。
3. 步骤 [3] 是判别段：放行文件在 [3] 返回**之后**才写，所以 [3] 整段（含首次 `WouldBlock`）都在持锁窗口内；步骤 [4] 的 `TimedOut` 再把「整段持锁」钉死（持有者只释放一次，且释放的必要条件——放行文件——在 [4] 之后才出现）。
4. 用例中的 sleep 只出现在**标记轮询**里（等 `holder.ready`、子进程等 `holder.release`），它们是握手标记的读取间隔，不承担任何排序假设；父进程没有任何依赖时长的猜测。
5. 判别断言 `waited + 50ms >= 250ms` 由被测循环的退出条件直接推出（§1.3），不是对时序的经验估计。

---

## 4. 证据（命令 + 关键原始输出）

所有命令均以 `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&` 为前缀；轮次脚本 `.recon/s13-raw/loop.sh`。

### 4.1 修复态：新用例连续 6 轮（≥5 要求）

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- release_probe_waits_out_an_external_holder
```

```text
run=01 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.46s
run=02 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.41s
run=03 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.29s
run=04 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.30s
run=05 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.31s
run=06 exit=0 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.29s
```

这 6 轮跑在**后来被还原成同一字节**的版本上：跑完后（临时改动之前）的 sha256 为
`f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7`，与最终文件一致（§4.6）。

### 4.2 证伪：把共享入口退回原调用点的零等待写法 → 3/3 红

临时改动（唯一一处，随后字节级还原）：

```diff
 async fn probe_initialization_lock_release(path: &Path) -> Result<std::fs::File> {
-    lock_schema_open(path, LOCK_RELEASE_PROBE_BUDGET).await
+    lock_schema_open(path, Duration::ZERO).await
 }
```

```text
run=01 exit=101 test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.02s
run=02 exit=101 test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.06s
run=03 exit=101 test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.09s
```

`helper-reverted-run-1.log` 原文：

```text
thread 'sessions::sqlite_store::connection::open_tests::release_probe_waits_out_an_external_holder' (110643070)
  panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:639:5:
release probe gave up before its budget while the holder still held the lock: waited 484.209µs < 250ms
```

还原（`cp` 自改动前副本）：

```text
before : f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7
after  : f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7
diff -q  → (无输出) RESTORE_OK
```

还原后再跑一次同 filter：`exit=0 / ok. 1 passed ... finished in 0.35s`（`restored-check.log`）。

### 4.3 修复态真实等待时长（测量探针）

把判别断言的右值临时改成不可能的 `Duration::from_secs(10)`，让失败消息打印真实 `waited`：

```text
panicked at ...connection_open_test.rs:639:5:
release probe gave up before its budget while the holder still held the lock: waited 252.794167ms < 250ms
```

结论：修复态等待 **252.794 ms**（≥ 250 ms 预算）；零等待回退实测 **0.484 ms**。两者相差约 520 倍，断言阈值为 `250ms − 50ms = 200ms`。探针改动随后整体还原（sha256 同上）。

### 4.4 边界实验：250 ms 是容忍窗口（复刻 S12 §4.3 方法）

在子进程 `initialization_lock_holder_child_process` 的 `drop(file)` 前临时插入真实持锁延时（唯一一处）：

```text
+ std::thread::sleep(Duration::from_millis(350));
run: exit=101 test result: FAILED. 0 passed; 1 failed ... finished in 0.56s
panicked at ...connection_open_test.rs:654:13:
release probe did not acquire after the holder released: session database initialization is busy
```

```text
+ std::thread::sleep(Duration::from_millis(100));
run: exit=0 test result: ok. 1 passed; 0 failed ... finished in 0.44s
```

即：100 ms 的合法持锁窗口被容忍，350 ms 的窗口被击穿；边界落在 (100 ms, 350 ms]，与 250 ms 预算量级一致。两次注入均已还原（sha256 同上）。这证明该预算**是容忍窗口而非机制修复**：它没有消除 `fork` 继承 fd 让锁在进程外存活的机制，只降低了它的可见范围。

### 4.5 全量 `-p peri-resources --lib`（基线 499 passed / 26 ignored）

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
```

```text
run1 exit=0 test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 16.48s
run2 exit=0 test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 17.72s
run3 exit=0 test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 17.96s
```

（1:1 替换用例，条数不变：499 + 26 = 525 = 过滤跑中 `524 filtered out` + 1。三份日志 `full-lib-run-{1,2,3}.log` 中 `test ... ok` 行数均为 499。）

### 4.6 规模与格式

```text
wc -l peri-resources/src/sessions/sqlite_store/connection_open_test.rs → 662
./scripts/cargo-rmcp-patched.sh fmt -p peri-resources -- --check      → exit 0
最终 sha256 → f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7
```

`git status --short` 中本任务范围内的改动只有该文件；其余 `M`/`??` 条目来自 S12 交付物与并行的 S13 其他任务（未触碰）。

### 4.7 残余边界实测：绕过共享入口（四处调用点各自内联零等待）

把四处调用点临时改成原 S12 前的写法（`lock_schema_open(&path, Duration::ZERO).await.unwrap()` 等），共享入口保持原样、仍被新用例使用：

```text
EXIT=0
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.29s
```

即：**调用点级绕过共享入口的写法不会被本用例抓住**。这是本轮结构上的残余缺口，已在 §5 列出。改动随后整体还原（sha256 同上）。

---

## 5. 未验证项与局限

1. **调用点级绕过不覆盖**（§4.7 实测绿）。守卫对象是「共享入口的等待语义」；预算现在只有一处实现，但调用点仍可自行内联零等待。该写法会在 diff 中留下调用点级的时长字面量（共享入口对该调用点随即失效），但本用例不会红。
2. **子进程侧获取 helper 无确定性证伪**。`acquire_initialization_lock_within`（两个子进程用例）仍是 S12 修复的一部分，但它在生产中争用的来源是**父进程 fork 时复制出去的 fd**，其存在与释放时机由并发测试决定，父进程既观察不到子进程的首次尝试、也无法把放行排在尝试之后，因此本用例无法对这条路径给出确定性红/绿证据。S12 §4.2 把子进程获取退回 `try_lock().unwrap()` 后新用例 3/3 绿（它不经过该路径），S12 §2.2 的 20 轮统计里该通道曾 2 次命中——即这条通道只能靠负载碰，不能确定复现。本轮未重做该实验。
3. **判别阈值依赖一个下界断言**。`waited + 50 ms >= 250 ms`：只有把零等待分支拖慢到 200 ms 以上才会漏报（实测 0.484 ms，余量约 400×）；反之修复态若出现异常提前返回（非 `WouldBlock` 的真实错误）也会红。两者本机未观察到。
4. **步骤 5 的绿依赖持有者在预算内反应**（§4.4：100 ms 绿、350 ms 红）。极端调度延迟（>250 ms）会让本用例假红，这与该预算本身的容忍窗口同源；未做长时间稳定性矩阵。
5. **平台与规模**：仅在 macOS（本机）与当前工具链运行；未覆盖 Linux/Windows、CI、`--test-threads` 压测、以及 S12 曾做过的 30 轮全量 soak。本轮为 6 轮 filter + 3 轮全量。
6. **未跑 clippy**、未跑其他 crate 的测试、未跑文档测试或 E2E；只对 `peri-resources` 做了 `fmt --check`（exit 0）。
7. **不涉及产品锁机制**：本用例与本次改动都不修复、也不验证「`fork` 继承 fd 导致锁在进程外存活」这一根因；它只固定测试侧释放探测的等待语义。
8. **报告未上传、日志在 `.recon/s13-raw/`**（工作树内，未提交）。

---

VERDICT: LOCK-TEST-DETERMINISTIC
