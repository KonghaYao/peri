# S9c — 回放标记门改按「当前连接」协商能力（附 S9 复核 R1/R2/R8/R9 收尾）

- 日期：2026-10-14
- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（**未提交**，`HEAD = 5be107603`）
- 驱动：`.recon/S9-verification.md` §7.1（反例）、§8（残余观察 R1/R2/R8/R9）、§10（结论）；`.recon/S9-boundedness.md` §6 R8
- 需求锚点：REQ §5.3「能力版本以**当前连接**的协商结果为准，不能继承会话内保存的旧客户端能力」；验收 A25
- 命令一律 `cd <工作树> && ./scripts/cargo-rmcp-patched.sh ...`（无裸 cargo、无 `--workspace`、无 `--no-verify`）

## 0. 结论摘要

1. §7.1 反例已关闭：回放标记（`periReplay` / `periLoadGeneration`）与响应代次戳的门，改读**本连接** `initialize` 的协商结果（`ConnectionFacts::caps`），不再回读会话级 `caps_registry`。
2. 反例探针的 `#[ignore]` 已去掉，转为回归用例；另新增 1 条正向用例，证明门没被改死（§4.2、§4.3）。
3. 负向复现做了一次：把门临时改回「不看连接」→ **只有**那条反例用例失败，报出 `["periLoadGeneration x5", "periReplay x4"]`；随后按 SHA-256 还原并复跑绿色（§4.4）。**没有**任何其他用例因此失败。
4. R1 / R2 doc 按用户要求收紧（**只改文档**：不加校验、不改语义）；R9 指针修正；R8 删掉依赖 tracing 全局调用点兴趣缓存的日志文本断言，产品断言保留。
5. 4 条验收命令 + 1 条辅助命令全部实测通过：`check`、`-- history_page_wire`（10 passed）、`-- replay` 连跑 3 次（各 32 passed，无 flake）、全量 `-- lib`（881 passed / 0 failed / 0 ignored）。
6. 无 DDL、无新增依赖、无 `Cargo.toml` / `Cargo.lock` 改动；工作区保持未提交。

## 1. 范围

### 1.1 覆盖（本次实际做的事）

1. `ConnectionFacts` 增加**本连接** caps 字段；`session/load` 分派把整份 facts 传进 `handle_load`；`handle_load` 只把**回放相关** caps 换成连接 caps（两个 `replay_*` 调用 + 响应 `_meta.periLoadGeneration`）。
2. `send_available_commands_update` **刻意**保持会话级并加注释说明（另一个功能面）。
3. 反例探针转回归 + 新增正向用例 + 私有 helper（`history_page_wire_test.rs`）。
4. R2 doc 收紧：`length(CAST(content AS BLOB))` 关掉 length-arg 优化 → 引擎仍可能把候选行正文读进寄存器（含那条永不被返回的 lookahead 行）；但表达式**不越过 SQL 边界、不进解码器**，返回集与解码字节不变。
5. R1 doc 收紧：追加只要 `rowid > tail` 就不进本次遍历；`tail` 行被删后 `rowid` 复用是**已知边界**（契约不保证、**不加校验**）。
6. R9 指针修正：`remote/session_history_segment_test.rs`（不存在）→ `remote/session_history_page_test.rs`。
7. R8 flake：删除 `LogBuffer` / `capture_logs` 与日志文本断言，改断言「调用记录 + 错误分类 Display 文本 + 交付内容」。

### 1.2 不覆盖（见 §5）

1. 真实外部 ACP 客户端（stdio 会话 / 第三方 SDK）的端到端。
2. Remote（云端）后端在 **ACP 层**的端到端（存储侧另有 remote 用例）。
3. 「两个真实连接先后接入同一进程」的并发时序实验（反例覆盖的是状态，不是时序）。
4. 性能预算重测（R2 的 I/O 代价只在 doc 里写明）。
5. `session/resume` 的命令广播仍走会话级注册表（判定为另一功能面，见 §3.4 第 4 条）。

## 2. 改动文件清单（绝对路径 = 工作树根 + 下列路径）

1. **`peri-acp/src/host/connection.rs`** — 本连接 caps 的唯一来源。
   - `:145-150` `facts()`：多填 `caps: self.caps()`（`history_pagination` 一行未动）。
   - `:153-164` `ConnectionFacts`：新增 `pub(crate) caps: PeriCaps`；derive 由 `Copy` 变 `Clone, Debug`（`PeriCaps` 含 `Vec<UiCommandSpec>`，非 `Copy`）；字段 doc 写明「不得回读会话级 `caps_registry`」。
   - `:133-144` 与 `:166-178` 两处 doc：说明两字段**来源不同、不可互相推导**；`unnegotiated()`（`#[cfg(test)]`）填 `history_pagination: false` + `caps: PeriCaps::all_enabled()`。
2. **`peri-acp/src/host/requests.rs`** `:120-124` — `session/load` 分派把整份 `ConnectionFacts` 交给 `session_lifecycle::handle_load`（注释说明两个字段的来源差异）。
3. **`peri-acp/src/host/requests/session_restore.rs`** — `handle_load` 的门控划分。
   - `:497-520`：签名 `connection: ConnectionFacts` + doc（一句话说明两个字段各管一处）。
   - `:534-541`：`let connection_caps = &connection.caps;`（回放面）与 `let session_caps = cfg.session_manager.ensure_session_caps(req_session_id);`（命令广播面）。
   - `:558` / `:580`：分流的门**仍是** `connection.history_pagination`（未改）。
   - `:586-603`：`replay_persisted_session_history`（协商）与 `replay_canonical_history_streamed`（未协商）都收 `connection_caps`。
   - `:633-640`：`send_available_commands_update(..., &session_caps, ...)`，上方注释「刻意会话级、不得为了一致而改成连接 caps」。
   - `:655-660`：响应 `_meta.periLoadGeneration` 的门由会话级改为 `connection_caps.replay`。
4. **`peri-acp/src/host/history_page_wire_test.rs`**
   - `:796-867`：helper `s9_recheck_unnegotiated_load(fixture, session_registry_caps)`——连接侧一律 `Some(PeriCaps::default())`（= 真 initialize 过但没声明任何 peri 扩展的标准客户端）。
   - `:918-945`：原 `#[ignore]` 反例探针，现为回归用例（会话级注册表写 `pagination_caps()`，断言 6 个私有键一个都不出现）。
   - `:947-1007` + `:1009-1042`：新 helper `s9_recheck_negotiated_load`（`select! { biased }`：通知优先取、load 结束即止）与正向用例 `s9_recheck_a_negotiated_connection_still_receives_the_replay_marks`（会话级注册表**刻意**写 `PeriCaps::default()`）。
5. **`peri-acp/src/host/requests_unnegotiated_replay_test.rs`** — R8。
   - 夹具新增 `connection_caps: PeriCaps`（默认 `PeriCaps::default()`）与 `set_connection_caps`；`load_unnegotiated` / `load_negotiated` 统一走 `load_with_pagination(bool)`；`set_caps`（会话级注册表）保留并注明只影响命令广播面。
   - `:733-771`：删 `LogBuffer` / `capture_logs` / 日志文本断言；改断言 `segment_calls().len() == 1`、`calls[0].error` 含 `"behavior is unsupported here"`、`full_reads() == 1`、交付内容等于 canonical。
   - 受影响用例改用 `set_connection_caps`：`streamed_canonical_replay_keeps_the_generation_gated_by_the_replay_capability`、`negotiated_initial_load_keeps_the_effective_set_and_the_entry_marks`、`s9_verification_negotiated_connection_still_carries_the_entry_marks`。
6. **`peri-acp-types/src/session_resources.rs`** — R1 / R2 doc 收紧（两阶段读取段 `:755-770` 一带；锚点消失与 `rowid` 复用边界）。
7. **`peri-acp-types/src/history_page.rs`** — R1 doc：模块文档追加/`rowid` 复用边界（`:73-78`）、`HistoryForwardCursor` 结构 doc、`tail` 字段 doc。
8. **`peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs:4`** — R9 指针修正。

未改：`Cargo.toml` / `Cargo.lock` / 任何 `.sql` / migrations。

## 3. 契约与不变量

### 3.1 `handle_load` 的门控表（改动后）

| 决定什么 | 判据 | 来源 | 本次 |
| --- | --- | --- | --- |
| 分流：协商 → 热态有效集合 + 条目位置戳；未协商 → canonical 流式 | `connection.history_pagination` | `ConnectionContext::history_pagination()`（未 `initialize` / 未声明 → `false`，**失败关闭**） | 不变 |
| 条目位置戳 `periEntryId` / `periPosition` | 同上（`positions: Option<_>` 决定 `ReplayMarks` 有没有位置） | 同上 | 不变 |
| 回放通知上的 `periReplay` / `periLoadGeneration` | `connection.caps.replay` | 本连接 `initialize` 协商值（未 `initialize` = `all_enabled`） | **改** |
| 响应体 `_meta.periLoadGeneration` | `connection.caps.replay` | 同上 | **改** |
| reminder 回放出口的事件选择 `system-reminder` / `system-reminder-fallback` | `connection.caps.system_reminder` | 同上（同一个 caps 参数，见 §3.5） | **改** |
| 命令表广播 `send_available_commands_update` | `ensure_session_caps(req_session_id)` | **会话级**注册表 | 不变（刻意） |

### 3.2 两个字段为什么并存、为什么不可互推

1. `history_pagination` 是**准入**判据：未协商必须失败关闭（REQ §5.1 / §5.3）。若改用 `caps.history_pagination`，进程内路径（TUI / MPSC，从未 `initialize`）会因 `caps()` 兜底 `all_enabled` 而被默认授予分页语义。
2. `caps` 是**展示**判据：未 `initialize` 的进程内路径必须保持历史全能力，否则 TUI 掉回放标记。
3. 两者来源不同（一个「显式协商 + 声明了分页」，一个「协商值或兜底全能力」），合成一个布尔必然在某一侧破坏语义；分歧点由 `peri-acp/src/host/connection_test.rs` 覆盖。

### 3.3 穷举：`periReplay` / `periLoadGeneration` 的全部生产发射点

命令与命中见 §4.6。结论：生产发射点共 3 处，现在都吃**连接 caps**：

1. `peri-acp/src/dispatch/session_replay.rs:596`（`replay_meta`，门 `caps.replay`）：content chunk / tool call / tool update 的 meta 都经它；整个模块内 `caps` 只在这里被消费（`project_message_updates` 其余部分不使用 caps）。
2. `peri-acp/src/host/requests/session_restore.rs:836`（`TuiReplaySender::send_system_reminder` 的代次戳，门 `caps.replay`）：`caps` 由调用链 `replay_* → send_projected → send_system_reminder` 传入，即 `connection_caps`。
3. `peri-acp/src/host/requests/session_restore.rs:658`（响应代次戳，门 `connection_caps.replay`）。

无关路径：`session/event_sink.rs` 的 live `push_system_reminder`（`replay=false`）不打回放标记，未改。

### 3.4 本次刻意不动

1. 分页的失败关闭方向（含位置戳读取失败 → 本次 load 失败）。
2. 未 `initialize` = `PeriCaps::all_enabled()`（TUI / MPSC 能力不缩水）。
3. 命令广播 `ui_commands` 仍走会话级注册表（`ui_commands` 明细在会话创建时注册进命令表）。
4. `session/resume` 不回放历史、不产回放标记，仍只用会话级 caps 广播命令表（`session_restore.rs:664-704`）。
5. R1 边界**不加校验、不改语义**（用户明确要求）。

### 3.5 门控变化带来的可见行为差异（须知）

1. 「先 peri 连接、后标准连接」依次 load 同一会话：标准连接现在**不再**收到 `periReplay` / `periLoadGeneration`（修复目标）。
2. 同场景若历史里有 client-audience 的 reminder：标准连接现在收到 `system-reminder-fallback`（纯文本 + `legacy: true`），而不是结构化 `system-reminder`。这是同一个「回放相关 caps」面的修正——否则结构化 Peri 事件同样是从会话注册表继承来的私有形状。
3. 协商连接行为不变（它的 `caps` 本来就等于协商值）；进程内 TUI / MPSC 不变（兜底 `all_enabled`）。

## 4. 实测证据

环境：工作树未提交，`HEAD = 5be107603`。8 个被改文件的 SHA-256（复核者可逐条比对；**本报告写入后再无代码改动**）：

```
05c4ad1ae06383619ad55941ddc125f61796ce160b0d9a4a9b99c69aecbf01b8  peri-acp/src/host/connection.rs
aa8fd1451f79d5ac50a02f82d19654eab96562f9ab82aec15ff8ed6c94f48d43  peri-acp/src/host/requests.rs
f15566daa65276a70dd499c42a9866ebbff06fd7cb24f82159c0c632ec1a2d2f  peri-acp/src/host/requests/session_restore.rs
4b74f1580ab46b297646b3ec6440da2e81c0f7a086b4e74ddeecb4330c519e18  peri-acp/src/host/history_page_wire_test.rs
165f85b52389bfdc3cdc0d5c24df6be218977befd93ad2d557e414331759ba22  peri-acp/src/host/requests_unnegotiated_replay_test.rs
2dcfd2a8652f8c4b98389742f4f0fa43b2383aa8dbbdde59725c1dfc49ffa71b  peri-acp-types/src/session_resources.rs
06b70c64bc47749bcb71681da7733c874ea7f2003c1a013d528cdcdab62d6743  peri-acp-types/src/history_page.rs
15217e97d285cdd24ea5082b965ab345c3b691b999d2eae3963c33aea8c150ca  peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs
```

（工作区未提交，`git diff` 展示的是整条特性切片，**不能**用它隔离本次改动；请按 §2 的 `file:line` 锚点定位。）

### 4.1 命令 1：编译

命令原文：

```
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp
```

结果原文（EXIT=0；全文 18 行 = 17 行 `Checking <crate>` + 下面这行；`grep -c "warning\|error"` 计数 0）：

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 29s
```

### 4.2 命令 2：分页 wire 面（含新增正向用例）

命令原文：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire
```

结果原文：

```
running 10 tests
test host::requests::tests::history_page_wire_tests::s9_recheck_a_negotiated_connection_still_receives_the_replay_marks ... ok
test host::requests::tests::history_page_wire_tests::s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection ... ok
test host::requests::tests::history_page_wire_tests::s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields ... ok
（其余 7 例均 ok，含 pagination_fields_from_an_unnegotiated_connection_are_rejected、a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor）
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 871 filtered out; finished in 11.13s
```

`0 ignored` = 反例探针不再是 `#[ignore]`。

### 4.3 命令 3：`-- replay` 连跑 3 次（R8 flake 对照）

命令原文（三次逐字相同，**串行**执行，无并发 cargo）：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- replay
```

| 第几次 | 结果原文（末行） | 备注 |
| --- | --- | --- |
| 1 | `test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 849 filtered out; finished in 4.15s` | 该次含编译，`Finished .* in 3m 47s` |
| 2 | `test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 849 filtered out; finished in 3.66s` | |
| 3 | `test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 849 filtered out; finished in 3.67s` | |

R8 用例（`streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported`）三次均为 `... ok`。**本轮 3 次全部通过，没有观察到失败**——所以「第几次失败」在本轮无对应记录；S9-boundedness §6 R8 记的那次失败（首跑只失败日志断言、产品断言两次都通过）是上一轮的观察，见 §6 风险 1。

### 4.4 命令 4：负向复现（证明反例用例真的能抓）

做法：把门临时改回「不看连接」——`connection.rs` 的 `facts()` 里 `caps: self.caps()` 改成 `caps: PeriCaps::all_enabled()`（一行），其余不动；跑 §4.2 同一条命令。

结果原文（EXIT=101）：

```
test host::requests::tests::history_page_wire_tests::s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection ... FAILED
（其余 9 例均 ok——包括新增的正向用例 s9_recheck_a_negotiated_connection_still_receives_the_replay_marks ... ok）

thread '...s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection' (94531779) panicked at peri-acp/src/host/history_page_wire_test.rs:941:5:
未协商分页的连接不得继承会话级 caps 带来的私有字段；实际命中: ["periLoadGeneration x5", "periReplay x4"]

test result: FAILED. 9 passed; 1 failed; 0 ignored; 0 measured; 871 filtered out; finished in 11.59s
```

还原：把那一行改回 `caps: self.caps()`，随后

```
shasum -a 256 peri-acp/src/host/connection.rs
05c4ad1ae06383619ad55941ddc125f61796ce160b0d9a4a9b99c69aecbf01b8
```

与 §4 指纹表逐字符一致（无残留），并复跑 §4.2：

```
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 871 filtered out; finished in 14.58s
```

结论：反例用例在「门不再看连接」时**必然失败**并直接点名泄漏的字段，说明它不是空转；且失败面精确收敛在该用例（正向用例仍 ok，证明门并非被改成「永远关闭」）。

### 4.5 命令 5：全量 lib

命令原文：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
```

结果原文：

```
running 881 tests
test result: ok. 881 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 264.18s
```

### 4.6 辅助命令：all-targets 编译（同一冻结树，先于 §4.1 执行）

```
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp --all-targets
```

结果原文（EXIT=0；文件 18 行，`grep -c "warning\|error"` = `0`）：

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 17s
```

### 4.7 静态核对（复核者可原样重跑）

```
grep -rn "caps\.replay\|periReplay\|periLoadGeneration" peri-acp/src --include=*.rs | grep -v "_test.rs"
```

→ 生产门只有 3 处：`session_restore.rs:658`、`session_restore.rs:836`、`session_replay.rs:596`（其余为 doc 注释）。

```
grep -n "LogBuffer\|capture_logs\|rebuild_interest_cache" peri-acp/src/host/requests_unnegotiated_replay_test.rs
```

→ 只剩 `:737` / `:738` 两行 **doc**（解释为何不断言日志），无代码残留。

```
ls -1 peri-resources/src/sessions/remote/session_history_page_test.rs
```

→ 存在（R9 修正后的指针目标）。

### 4.8 无 DDL / 无新依赖

```
git diff --name-only | grep -i "Cargo.toml\|Cargo.lock\|\.sql$"
git diff | grep -c "CREATE TABLE\|ALTER TABLE\|DROP TABLE"
```

→ 第一条**无输出**；第二条 `0`。

## 5. 未验证与不确定项

1. **真实外部 ACP 客户端**：本轮只跑进程内 MPSC 传输 + 真实 `ServerLoop` + 真实 SQLite 门面；没有起过真实 stdio 会话，也没有第三方客户端接入。§3.5 的行为差异在真实客户端上的观感未验证。
2. **Remote（云端）后端在 ACP 层**：`peri-resources` 的 remote 用例覆盖存储侧；ACP 层用例统一用 SQLite 门面，未在 ACP 层端到端跑过 remote。
3. **多连接时序**：反例覆盖的是「会话注册表已被写过 + 当前连接未协商」这一**状态**；没有做「两个真实连接先后/并发接入同一进程」的时序实验（例如 `initialize` 与 `session/load` 交错）。
4. **性能**：分页与回放的性能证据在 `.recon/perf-evidence.md`，本切片未重测。R2 提到的每段多一次 I/O 与瞬时内存是**文档化**的代价，未量化。
5. **`session/resume`**：仍只用会话级 caps 广播命令表。若复核者认为 REQ §5.3 也覆盖命令表广播，这里是一处未改面（我的判断：属另一功能面，见 §3.4 第 4 条）。
6. **R8 根因未被本轮独立复现**：本轮只证明「去掉日志断言后 3 连跑稳定」。S9-boundedness 的归因（tracing 调用点兴趣缓存被同进程并行用例搅动）来自上一轮观察；**没有**做「恢复旧断言 → 重新复现失败」的实验。
7. **未跑 doctest / 集成测试目标**：`--all-targets` 只到**编译**通过；`--lib` 覆盖 881 例（0 ignored），`peri-acp` 的集成测试目标未执行。
8. **`ConnectionFacts` 由 `Copy` 变 `Clone`** 后的开销未测（见 §6 风险 3）。

## 6. 风险

1. **丢掉了一条 warn 的回归保护**（R8 改动的主要代价）：生产代码「回退到全量读取」时的那句 `warn` 仍在，但不再有断言盯着它；新断言钉的是触发条件（首段返回 `Unsupported`）、调用次数与交付内容。要恢复日志覆盖，得先解决 tracing 断言在并行测试下的不稳定性（例如独立进程或串行化 callsite 无关的收集方式）。
2. **语义面比任务字面稍宽一点**：传入 replay 路径的 `caps` 参数同时决定 reminder 的事件名（§3.5 第 2 条）。我认为这是同一个「回放相关 caps」面的正确修正（否则结构化 Peri 事件会从会话注册表继承私有形状），但这是本次唯一「超出三处点名发射点」的行为差异，复核者若不同意，只需回看 `session_restore.rs:814-828`。
3. `ConnectionFacts` 变 `Clone` 后每次请求分派多一次 caps 克隆（含 `Vec<UiCommandSpec>`）；量级很小但未实测。
4. `ConnectionFacts::unnegotiated()` 是 `#[cfg(test)]` 构造：它把「未协商」固定为 `false + all_enabled`。若将来生产出现新的「无连接上下文」调用点，测试夹具语义可能与生产语义分叉（doc 已写明）。
5. **会话级注册表仍在被写**（`ensure_session_caps`）：本次只保证「回放面不再读它」。若将来在**另一个**发射点重新引入会话级读取，现有反例用例抓不到——需要在每个新发射点重复同样的门控判据（§3.3 的穷举命令可作为复查入口）。

## 7. 独立复核步骤（复制粘贴）

1. `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
2. 指纹比对：`shasum -a 256 <§4 列出的 8 个路径>`，逐条与 §4 的指纹一致。
3. 跑 §4.1、§4.2、§4.3（3 次）、§4.5 四条命令，比对结果原文（`-- replay` 的用时允许浮动，`passed/failed/ignored/filtered` 计数必须一致）。
4. 跑 §4.7 三条 `grep` / `ls`，比对命中集合。
5. 读 §3.1 表格，逐行到 §2 给出的 `file:line` 对照代码；重点确认 `:558` / `:580` 仍是 `connection.history_pagination`，`:640` 仍是 `&session_caps`。
6. 想亲自复现反例（可选，会临时改代码）：把 `peri-acp/src/host/connection.rs` 的 `caps: self.caps()` 改成 `caps: PeriCaps::all_enabled()`，跑 §4.2 命令，预期**只有** `s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection` 失败并报出 `periReplay` / `periLoadGeneration` 计数；改回后 `shasum -a 256` 应回到 `05c4ad1a…01b8`。
