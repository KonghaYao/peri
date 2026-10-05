# Wave 3：依赖剥离 debug info（dev profile）——实施、A/B 测量与结论

状态：**已实施**——根 `Cargo.toml` 新增 `[profile.dev.package."*"] debug = false`，workspace 成员保留 `debug = 1`。本机并发环境使 CPU/墙钟无法可靠分离，确定性收益（磁盘、体积、语义）成立并多轮复现。

测量位置：worktree `.worktrees/w3-profile`（分支 `perf/w3-profile`，基于 `9537e37c`，含 Wave 1+2）。日期：2026-10-05。机器：macOS 26.x、Apple Silicon 18 核、48GB；rustc/cargo 1.99.0。所有 cargo 命令经 `scripts/cargo-rmcp-patched.sh`；测量用独立 `CARGO_TARGET_DIR=/tmp/peri-w3-profile-{base,new}`；并发探针每 5s 记录 `ps` 汇总（采样窗口 = 构建全程）。

对应关系：主报告 `spec/issues/2026-10-05-rust-build-performance-diagnosis.md` Wave 1 §7.1 A2 的后续；Wave 2 报告 `…wave2-deps.md` §5.2「依赖 crate debug=0（未采信，待干净复测）」的正式复测与采纳。

## 摘要

| 项 | 结论 | 证据强度 |
| --- | --- | --- |
| 配置实现 | `[profile.dev.package."*"] debug = false`（+1 注释行，共 3 行） | — |
| 覆盖语义 | 依赖（含 proc-macro / build script）`debuginfo=0`，workspace 成员 `1` | 双证据（探针 + 9 轮全量构建 `-v` 抽样） |
| test profile | **自动继承** dev 的该覆盖（实测依赖 `debuginfo=0`），无需额外配置 | `cargo check --tests` 实测 |
| 磁盘 | clean dev target **4.3G → 3.8G（-11.6%）**；其中 deps 2.8G → 2.3G（-0.5G / -17.9%） | 9 轮 clean 构建，同配置逐字节一致 |
| dev 二进制 | **224,382,184 B → 215,253,800 B（-9.13MB / -4.07%）** | 6 个独立样本，同配置完全一致（±0） |
| CPU | 3 组配对 Δ 介于 -17.3% ~ +11.5%，方向不一致；**并发噪声（±12%+）覆盖配置效应，无法分离** | 9 轮 clean 构建 + 全程并发采样 |
| 墙钟 | **不可测量**（外部构建反复出现；同配置跨轮 -0% ~ +150%） | 同上，附原始数据与并发标注 |
| CI 交互 | **不变**：CI 已用 `CARGO_PROFILE_DEV_DEBUG=0`/`CARGO_PROFILE_TEST_DEBUG=0` 全局覆盖；本地 test 与 CI 行为更一致 | 见 §四 |

## 一、改动与语义验证

### 1.1 改动（根 `Cargo.toml`）

```diff
 [profile.dev]
 debug = 1
+
+# Dependencies carry no debug info in dev; workspace members keep debug=1.
+[profile.dev.package."*"]
+debug = false
```

### 1.2 profile 覆盖语义（`cargo build -v` 抽样，实证）

用独立目标（`-p peri-time`，`--config` 注入同一配置）先做语义探针，再用全部 9 轮全量构建的 `-v` 日志交叉验证（逐条解析 rustc 命令行的 `-C debuginfo=`）：

| 轮次 | 依赖（registry/git） | workspace 成员 | 结论 |
| --- | --- | --- | --- |
| 探针（`peri-time`） | 全部 `debuginfo=0`（含 `tokio_macros` 等 proc-macro、build script） | `peri_time` = 1 | 覆盖只作用于非成员 |
| 全量 base 轮（A1/A3/A4/A5，`-v`） | 153 个 `0` 为 build script / 宿主单元；其余全部 `1` | 24/24 = 1 | base 语义（未改动前） |
| 全量 new 轮（B1/B2/B3/B4/B5，`-v`） | **562 个 `0`（全部依赖）** | 24/24 = 1 | 覆盖生效且不波及成员 |

- `debug = false` 即 `debuginfo=0`；`[profile.dev] debug = 1` 仍作用于 workspace 成员（Wave 1 的收益保留）。
- 关键语义确认：**`package."*"` 不覆盖 workspace 成员**，无需调整写法（即「依赖不带 debug info、成员带」）。
- 依赖侧含 proc-macro 与 build script（`debuginfo=0`），它们运行时的栈回溯同样无符号。

### 1.3 test profile：自动继承（实测，无需额外配置）

在修改后的配置下执行 `cargo check --tests -v -p peri-time`（test profile，`--test` 目标已确认）：

- 依赖 23/23 = `debuginfo=0`；成员 `peri_time` = 1。

即 `[profile.test]` 继承 `[profile.dev]` 的 package 覆盖，**本地 `cargo test` 的依赖同样不带 debug info**，无需再加 `[profile.test.package."*"]`。与 CI 的 `CARGO_PROFILE_TEST_DEBUG=0`（全局含依赖）方向一致，本地/CI 差距收窄。

## 二、测量设计

- 命令（两配置完全一致）：`CARGO_TARGET_DIR=/tmp/peri-w3-profile-{base,new} ./scripts/cargo-rmcp-patched.sh build -v -p peri-tui --bin peri --timings`，每轮独立 target、先 `rm -rf` 再 clean 构建。
- `/usr/bin/time -l` 取 `real`（墙钟）与 `user+sys`（实测 CPU）；`--timings` 的 Σduration 含排队/IO 等待（Wave 2 §5.1：是实测 CPU 的 1.9~2.6 倍），**本文不使用 Σduration 作 CPU 口径**。
- 并发探针：构建全程每 5s 记录 `ps -Ao %cpu=` 总和与 rustc 进程数，用于标注每轮环境（见 §3.2）。
- 两配置切换：base = `git show HEAD:Cargo.toml` 临时恢复（构建后校验恢复），new = 工作区修改版。
- 环境事实：测量窗口内**外部构建反复出现**（`/tmp/peri-ci-validation`、`/tmp/peri-verify-target`、build-perf worktree 的 check/clippy/test 等对同一台机器并发），多数轮次受不同程度污染；仅个别短窗口完全空闲。
- 另有一次配置切换时序错误的构建（目标目录标为 base 实为 new 配置）已作废、未计入本文任何统计。

## 三、结果

### 3.1 确定性指标（与负载无关，多轮一致）

| 指标 | base（deps debug=1） | new（deps debug=0） | Δ |
| --- | --- | --- | --- |
| clean target 总量 | 4.3G（A1/A4/A5 三轮一致） | 3.8G（B1/B2/B4/B5 四轮一致） | **-0.5G / -11.6%** |
| `debug/deps` | 2.8G（A4） | 2.3G（B2/B4 一致） | **-0.5G / -17.9%** |
| `debug/incremental` | 1.2G（A4） | 1.2G（B4） | 相当（跨轮波动 1.2~1.6G，未取得稳定对比） |
| dev 二进制（`target/debug/peri`） | 224,382,184 B（A1/A4/A5 一致） | 215,253,800 B（B1/B2/B4/B5 一致） | **-9,128,384 B / -4.07%** |
| `--timings` 编译单元数 | 642 | 642 | 相同（debuginfo 不改变单元划分） |

二进制字节数与 target 总量在同配置多轮完全一致（A1=A4=A5、B1=B2=B4=B5），是本次改动**最强的确定性证据**。

### 3.2 CPU 与墙钟（原始数据，附并发标注）

每轮 clean 构建（按时间顺序）：

| 轮 | 配置 | real（s） | user（s） | sys（s） | CPU=user+sys | avg_cpuload（%） | max_rustc | rustc>18 采样 | 环境标注 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A1 | base | 116.73 | 367.14 | 56.68 | 423.8 | 370 | 16 | 0 | 无外部 rustc 竞争 |
| B1 | new | 126.75 | 406.12 | 70.37 | 476.5 | 568 | 15 | 0 | 无外部 rustc；系统其他负载较高 |
| B2 | new | 225.52 | 426.09 | 66.46 | 492.6 | 605 | 34 | 16 | 重污染（另一 owner 全量 test 并行） |
| B3 | new | 142.17 | 390.23 | 63.68 | 453.9 | 567 | 21 | 1 | 前段外部 rustc ~10 |
| A3 | base | 116.56 | 406.83 | 65.02 | 471.9 | 457 | 15 | 0 | 无外部 rustc |
| A4 | base | 173.01 | 410.93 | 62.29 | 473.2 | 604 | 18 | 0 | 无外部 rustc；系统其他负载高 |
| B4 | new | 169.83 | 451.69 | 75.80 | 527.5 | 667 | 16 | 0 | 无外部 rustc；系统其他负载高 |
| A5 | base | 208.27 | 473.01 | 72.54 | 545.6 | 797 | 50 | 11 | 重污染（后半段外部构建爆发） |
| B5 | new | 151.66 | 387.06 | 63.92 | 451.0 | 544 | 16 | 0 | 无外部 rustc |

说明：`avg_cpuload` = 构建全程 5s 采样中 `ps -Ao %cpu=` 全系统之和的平均（含非 rustc 负载）；`rustc>18 采样` = 采样时刻 rustc 进程数 > 18（物理核数）的次数，是「确有外部 rustc 并行」的下界证据。

同配置跨轮差异（噪声量级）：

- base：real 116.56~208.27（+79%）；CPU 423.8~545.6（+28.7%）。
- new：real 126.75~225.52（+78%）；CPU 451.0~527.5（+17.0%）。
- 两配置的 CPU 分布**重叠**（new 最低 451.0 低于 base 最高 545.6）。

背靠背配对（同一窗口内先后构建）：

| 配对 | 顺序 | real Δ（new−base） | CPU Δ（new−base） | 配对期负载对称性 |
| --- | --- | --- | --- | --- |
| Pair 1 | B3 → A3 | +25.6s（new 慢 22%） | −17.9s（new 低 3.8%） | 不对称（new 前段遇外部 rustc） |
| Pair 2 | A4 → B4 | −3.2s（new 快 1.8%） | +54.3s（new 高 11.5%） | 较对称（两边均为中高系统负载） |
| Pair 3 | A5 → B5 | −56.6s（new 快 27%） | −94.6s（new 低 17.3%） | 不对称（base 后半段重污染） |

**结论（CPU/墙钟）**：

- 3 组配对的 CPU Δ 方向不一致（-17.3% ~ +11.5%），全部落在并发噪声区间内；墙钟 Δ（-27% ~ +22%）与 CPU 方向亦不一致（如 Pair 1 墙钟与 CPU 反向）。
- 本机测量窗口内并发噪声（同配置跨轮 ±17%~28% CPU、±79% 墙钟）大于配置效应（预期 ≤4% CPU），**本次无法证实也无法证伪 deps debug=0 的 CPU/墙钟收益**。这与 Wave 2 §5.1/§5.2 的发现一致（同一台机器、同类外部负载）。
- 参考：Wave 2 的受污染样本 D（deps debug=0）相对基线 C 为 CPU -3.5%、墙钟不可比。本 wave 最干净的一对可比性有限（B5: CPU 451.0 / avg 544 vs A1: 423.8 / avg 370），不作为结论。
- 结论按确定性指标（磁盘、二进制、语义）采纳；CPU/墙钟待独占机器复测。

### 3.3 单元级证据

- 单元集合与数量两配置相同（642 单元），差异只在依赖单元的调试信息产出。
- `-v` 抽样确认：同一 crate（如 `serde`/`tokio`/`rmcp`）在两配置下的 rustc 命令行仅在 `-C debuginfo=`（1 vs 0/none）不同。
- 链接阶段：new 的最终链接输入（`.rlib` 无 DWARF）更小，链接产物二进制 -4.07%。

## 四、影响与交互

1. **调试影响**：依赖代码（含 proc-macro/build script）的栈回溯不再有符号/行号；panic 若发生在依赖内部，backtrace 只显示地址/无源码行。workspace 成员仍为 `debug=1`（行号表），自有代码调试不受影响。若需临时调试某依赖，可用 `[profile.dev.package.<name>] debug = 1` 单独开启。
2. **CI 交互**：`ci.yml` 已有 `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0`（env 覆盖 profile 顶层），CI 行为**不变**（原本就是全局无调试信息，含依赖）。本改动只影响本地（未设 env 时）。
3. **test 交互**：本地 `cargo test` 的依赖自动 `debug=0`（§1.3 实测），构建更快更小、与 CI 一致；测试本体（成员 crate 的 test 目标）仍 `debug=1`。
4. **增量构建**：依赖 fingerprint 变化→**首次应用后旧 target 会有一次依赖全量重编**（一次性，与 Wave 1 同样的机制）；此后依赖单元不含调试信息，写入量更小。
5. **其他 profile**：`release` 不受影响（本改动仅 dev）。wasm/emscripten 目标同属 dev profile，规则一致。

## 五、回退

- 删除根 `Cargo.toml` 中 3 行（`[profile.dev.package."*"]` 与 `debug = false`，含注释）即可；无源码/数据副作用。删除后首次构建依赖全量重编一次。

## 六、未验证项与限制

1. **CPU/墙钟收益未分离**（§3.2），本机并发环境是硬约束；建议在独占机器/CI 空闲窗口复测（两配置各 ≥3 轮交错，用 `user+sys` 口径）。
2. `debug/incremental` 大小跨轮波动（1.2G~1.6G），未取得稳定对比；deps 与总量的对比稳定。
3. 只覆盖 dev/test profile；release 与 bench 未涉及（release 本就不带 debug 影响行为……其 debug 设置未变）。
4. 测量为单机（macOS/18 核），Windows/Linux 未测；该配置为平台无关的 Cargo 语义。
5. 测量期间外部并发构建不可控（详见 §二），本文所有结论均以「确定性指标」或「附并发标注的时间指标」表述。

## 附录 A：复现命令

```bash
cd .worktrees/w3-profile

# new（当前工作区）：clean dev 构建 + 计时 + 并发采样
CARGO_TARGET_DIR=/tmp/peri-w3-profile-new bash -c 'time ./scripts/cargo-rmcp-patched.sh \
  build -p peri-tui --bin peri --timings'

# base：临时恢复 HEAD 版 Cargo.toml
git show HEAD:Cargo.toml > /tmp/base.toml && cp Cargo.toml /tmp/new.toml && cp /tmp/base.toml Cargo.toml
CARGO_TARGET_DIR=/tmp/peri-w3-profile-base bash -c 'time ./scripts/cargo-rmcp-patched.sh \
  build -p peri-tui --bin peri --timings'
cp /tmp/new.toml Cargo.toml

# 并发采样（构建同时执行，5s 一次）
while :; do printf '%s ' "$(date +%s)"; ps -Ao %cpu= | awk '{s+=$1} END {printf "cpuload=%.0f ", s}'; \
  ps -Ao comm= | grep -c rustc; sleep 5; done

# 语义抽样（在构建的 stderr 日志上；`-v` 输出）
python3 -c '
import re,sys,collections
pat=re.compile(r"--crate-name ([A-Za-z0-9_]+)"); dbg=re.compile(r"-C debuginfo=(\d+)")
rows=[(pat.search(l).group(1),(dbg.search(l).group(1) if dbg.search(l) else "0"))
      for l in open(sys.argv[1]) if "rustc" in l and "--crate-name" in l]
print(collections.Counter(d for _,d in rows))' /tmp/w3-b5.cargo.stderr

# 磁盘与体积
du -sh /tmp/peri-w3-profile-base /tmp/peri-w3-profile-new
ls -l /tmp/peri-w3-profile-{base,new}/debug/peri

# test profile 继承验证
CARGO_TARGET_DIR=/tmp/peri-w3-testsem ./scripts/cargo-rmcp-patched.sh check --tests -v -p peri-time
```

## 附录 B：测量日志位置（临时文件，可复现生成）

`/tmp/w3-a{1,3,4,5}.{time,pslog,cargo.stderr}`、`/tmp/w3-b{1,2,3,4,5}.*`、`/tmp/w3-testsem.stderr`、`/tmp/peri-w3-profile-{base,new}/cargo-timings/*.html`。
