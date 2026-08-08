# E2E: compact-command 截图时机过早 + manual /compact 完成提示在 replay 后丢失

**状态**：Fixed
**优先级**：中
**类型**：产品缺陷（SystemNote 跨 replay 丢失）+ 测试鲁棒性（截图时机）
**创建日期**：2026-08-08
**来源**：E2E 全量运行（2026-08-06 第二轮，`e2e/e2e-results-2026-08-06-2.md` 问题 2）

## 问题描述

`e2e/tests/scenarios/compact-command.test.ts` 失败（50s，未重试）。失败点：`tests/scenarios/compact-command.test.ts:74`，`expect(result.pass).toBe(true)`（Judge 名 `/compact`）。

- Judge 反馈：check 1/3 通过（状态栏 `8% 200k` 符合百分比格式、输入框可见可用），但 check 2 失败——"在用户输入 '用中文简短回复: 今天天气不错' 的响应下方，出现了大量无意义的、重复的空白行和 ANSI 转义序列，占据屏幕中部大量空间，导致布局错位和视觉混乱"。
- 该测试历史：2026-08-01 全量（57.3s ✅）、2026-08-06 首轮（53s ✅）、2026-08-06 第二轮（50s ❌）——3 次运行 2 过 1 挂，失败与通过录制备查布局同构。

## 根因分析（2026-08-08，两阶段）

### 阶段一（初判）：测试等待时机缺陷

**测试等待时机缺陷，非产品渲染 bug。** 证据链：

1. 失败录制（`e2e/recordings/compact-after.txt`）显示消息区顶部为 `✶ 学以致用 (3s · ↓ 15k tokens)`——`✶` 是 spinner 动画帧（`peri-tui/src/components/spinner/mod.rs` `render_to_lines`），`(3s · ↓ 15k tokens)` 为 elapsed + token 数后缀。这是 **spinner 进行中**状态。
2. 截图时消息区**未折叠**（compact 前 2 轮对话完整保留），且**无"压缩完成"SystemNote**（`handle_compact_completed` 注入 `app-note-compact-completed-summary`，zh 文案形如"完整压缩完成"）——compact 事件链（CompactCompleted → SystemNote → TurnDone → spinner 停止）未走完。
3. 测试在第二次 Enter 后仅固定 `sleep(500) + sleep(3000)` 共 3.5s 即截图；compact 全量处理（真实 LLM 摘要）耗时通常 > 3.5s。
4. compact 处理中消息区内容被清空/折叠，屏幕出现大量留白——Judge 将"处理中 + 稀疏布局"误判为"渲染异常/ANSI 残留"（原始 raw 中的 ANSI 序列实为终端正常渲染产物，recorderConfig.ansi 未开启故未留存）。
5. before/after 录制布局同构（before 同样存在留白），功能证据全部正常（状态栏 `8% 200k` 说明压缩确实执行、输入框 `❯` 可见）——符合"等待时机"而非"渲染缺陷"的判定。

按此假设修复测试等待时序后（waitFor"压缩完成"），**e2e 复跑仍失败**，暴露真实产品缺陷：

### 阶段二（真相）：manual /compact 完成 SystemNote 在 replay 后丢失 + 重注入死锁

复跑证据链（`.tmp/agent-tui.log`，session 019fdf22/019fdf28）：

1. compact 实际在 ~12s 内完成（`CompactCompleted` 02:11:24），但"压缩完成"提示**从未出现在屏幕上**——`waitFor` 120s 超时。
2. 机制分析：manual /compact 完成提示经 `inject_system_note` 进入 `current_turn` → `TurnDone` 触发 flush 归档到 `committed` → `compact_just_completed` 触发 `THREAD_LOAD_TX` → `thread_load_consumer` 调 `load_session()` → **BRIDGE_RESET_COUNTER 递增 → bridge reset 分支清空 `committed`（含刚归档的 SystemNote）** → replay 事件到达时提示已消失。
3. 修复：新增全局 atom `PENDING_COMPACT_NOTE`（`peri-tui/src/kit/atoms.rs`），`handle_compact_completed` 在 manual 分支写入（auto compact 不写，避免残留串到后续 thread 切换）；bridge reset 分支在 session filter 之后消费该 atom、重建 SystemNote 到 `current_turn`（push_system_note），随即清空 atom。
4. **二次缺陷（死锁）**：初版重注入代码写为 `if let Some(x) = atoms::PENDING_COMPACT_NOTE.state().read().clone()`——Rust 临时值生命周期规则使读锁 guard 存活至整个 if-let 块结束，分支体内 `set(None)`（写锁）与仍持活的读锁在同一线程上死锁（std RwLock 非重入）→ bridge 事件循环停摆、`load_session()` await 永久挂起（复现：手动 tmux 会话 019fdf28 与 e2e 第二次运行同构）。
5. 修复：先以显式块 `{ let guard = ...; guard.clone() }` 提取值、guard 立即 drop，再进 if-let；分支内 set 不再竞争。手动验证（session 019fdf2e）日志链完整：
   ```
   [COMPACT_NOTE] reset branch: checking pending compact note
   [COMPACT_NOTE] pending note found, injecting
   bridge: re-injected compact completion note after replay reset
   ```
   屏幕显示 "Full compaction completed — This session continues from a previous conversation..."，SystemNote 跨 replay 存活。

## 修复内容

### e2e 测试（阶段一修复，保留）

`e2e/tests/scenarios/compact-command.test.ts`：

- 第二次 Enter 后，固定 sleep 改为 **`waitFor` 屏幕出现"压缩完成"文本**（compact 完成的确定性信号，SystemNote 注入；超时 120s，失败时明确报"完成提示未出现"而非 flaky 误判）。
- 随后 `waitForStableScreen(60s)` 等待 spinner 停止、TurnDone 后屏幕稳定，再截图。
- **locale 修正**：完成提示文案随 locale 变化（zh "压缩完成" / en "compaction completed"；e2e 环境 `LANG=C.UTF-8`，fluent 回退英文），waitFor 条件同时匹配两种文案。

### 产品修复（阶段二，`peri-tui`）

- `peri-tui/src/kit/atoms.rs`：新增 `PENDING_COMPACT_NOTE: AtomStatic<Option<String>>`（manual /compact 完成提示，跨 TurnDone → session/load replay 存活）。
- `peri-tui/src/kit/acp_events/compact.rs`：`handle_compact_completed` manual 分支写入 atom（auto 分支不写）。
- `peri-tui/src/kit/acp_bridge.rs`：reset 分支在 session filter 后消费 atom 重建 SystemNote；**先 clone 出值再进 if-let**（防读锁 guard 存活导致写锁死锁），消费后立即清空。

## 验证结果

- 手动验证：tmux repro（session 019fdf2e）——日志显示 re-inject 成功、屏幕可见 "Full compaction completed"，死锁消除、load_session 正常返回。
- 单元测试：`cargo test -p peri-tui --lib` 全绿（873 passed, 1 ignored），含 `test_compact_turndone_reload` 扩展场景（auto 分支 atom 为 None；manual 分支 atom 含提示文本）。
- 构建/静态检查：`cargo build -p peri-tui`、`cargo clippy -p peri-tui --all-targets -- -D warnings` 通过。
- E2E 终验：`npm test -- tests/scenarios/compact-command.test.ts` **通过**（62.8s；Judge 3/3：压缩完成提示 "Full compaction completed" 可见、输入框可用、状态栏 `8% 200k` 合理）。

## 涉及文件

- `peri-tui/src/kit/atoms.rs` —— PENDING_COMPACT_NOTE atom
- `peri-tui/src/kit/acp_events/compact.rs` —— manual 分支写入 atom
- `peri-tui/src/kit/acp_bridge.rs` —— reset 分支重注入（含死锁修复）
- `peri-tui/src/kit/acp_events_test.rs` —— 扩展 `test_compact_turndone_reload`
- `e2e/tests/scenarios/compact-command.test.ts` —— 场景测试（等待"压缩完成"信号）
- 佐证：`peri-tui/src/components/spinner/mod.rs`（spinner 进行中渲染）、`e2e/recordings/compact-after.txt`（失败录制）、`.tmp/agent-tui.log`（02:11-02:24 段日志链）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-08 | — | Open→Fixed | agent | 8-06 第二轮失败未登记；本次盘点补登 + 根因定位（截图时机过早）+ 修复（等待"压缩完成"信号），待 e2e 复跑确认 |
| 2026-08-08 | Fixed | Open→Fixed | agent | e2e 复跑暴露真实产品缺陷：SystemNote 被 replay reset 清空 + 重注入死锁；实现 PENDING_COMPACT_NOTE + reset 重注入 + 死锁修复，手动验证通过，待 e2e 复跑终验 |
| 2026-08-08 | Fixed | Open→Fixed | agent | e2e 复跑（locale C.UTF-8）暴露断言 locale 不匹配（屏幕为英文 "Full compaction completed"）；waitFor 改为同时匹配中英文，e2e 终验通过（62.8s，Judge 3/3） |

## 修复记录

### 2026-08-08 修复（agent）

- 阶段一：以"压缩完成"SystemNote 为完成信号替换固定 sleep（测试时序）。
- 阶段二：manual /compact 完成提示跨 replay 存活的完整机制（atom + reset 分支重注入）；重注入块以显式块提取值规避临时读锁 guard 存活造成的写锁死锁。
