# S11：F1（P1）修复记录 —— 初始游标锚点不再落在不可展示条目

- 日期：2026-10-14（CST）
- 触发：S10 对抗审计 `final-adversarial-audit.md` §1（F1，P1）
- 结论：**已修复 + 已证伪 + 已由独立复核者复核**（复核见 `.recon/S11-verification.md`）
- 分支/worktree：`feat/transcript-shrink-history-pagination` @ `.worktrees/peri-transcript-history`（改动仍未提交）

## 1. 缺陷

`peri-acp/src/dispatch/session_load.rs::initial_history_cursor` 取 `effective.payloads.first()` 当分页游标锚点，不检查可展示性。

- 子会话（subagent / fork / bg）自有区首条 = 身份 System 条目（`peri-agent/src/session/subagent/factory/spawn.rs:348-352`，全仓唯一 append System 的生产路径）。
- System 条目**不被 Full 排除**（`peri-agent/src/agent/compact_v2/full.rs:86-101`）、**永不进入客户端渲染**。
- 后果：这类会话一次 Full 之后 `nextCursor = null`（谎称没有更早历史）；即便锚点不是最旧行，比它晚的被排除原文也永远不被任何一页交付 ⇒ 压缩原文在 UI 完全不可达。

## 2. 修复（3 行）

```rust
let anchor = effective
    .payloads
    .iter()
    .find(|payload| payload.displayable_to_client())
    .or_else(|| effective.payloads.last())
    .map(PersistedPayload::id);
```

1. 可展示性判据 = `PersistedPayload::displayable_to_client()`（`peri-acp-types/src/store/mod.rs:66`）——与分页出口装配 `items` 的过滤**同一条规则**，不在 ACP 层另立判据。
2. 兜底：有效视图里一条可展示条目都没有（只有身份 System / model-only reminder）时退到最新一条自有 payload；它同样不在首屏展示，所以不重不漏；是否收尾仍由 `range_head` 判据决定（宁可多给一次空页，也不谎称没有更早历史）。
3. 未触碰：DDL / schema、JSON-RPC 形状、错误码、caps 门控、分页页规则。

## 3. 回归用例（`peri-acp/src/host/history_page_wire_test.rs`）

| 用例 | 形状 | 断言 |
| --- | --- | --- |
| `the_initial_cursor_anchors_past_non_displayable_own_rows` | own = [System 身份, h1, h2] + 一次 Full（排除 h1/h2、追加摘要） | 锚点 = 摘要（不是 System 条）；`nextCursor` 非空；页链按历史正序交付 `[h1, h2]`；与首屏零重叠；读到范围起点收尾（`nextCursor = None`） |
| `the_fallback_anchor_keeps_an_archived_row_reachable` | 有效视图 = [身份 System, 仅模型受众 reminder]（零可展示条目）+ 存储里仍有一条更老的可展示归档行 | 兜底锚点 = 最新一条有效自有条目；`nextCursor` 非空；该归档行被页交付。**形状需绕过 `full.rs` 直接调 `apply_compaction`**（「摘要被移除后正文仍被排除」的极端版），钉的是兜底分支的*选择*，不是生产可达路径 |
| `an_all_non_displayable_own_region_reports_no_earlier_page` | own = [System 身份] | 兜底锚点即范围起点 → `nextCursor = null`（不编造游标） |

三条都走真实 API：`crate::dispatch::initial_history_cursor` + `crate::dispatch::load_history_page`。

## 4. 证伪（原始证据）

把实现临时改回 `payloads.first()`（等价写法 `.take(1).find(|_| true)`）：

```
running 1 test
test host::requests::tests::history_page_wire_tests::the_initial_cursor_anchors_past_non_displayable_own_rows ... FAILED
thread '...' panicked at peri-acp/src/host/history_page_wire_test.rs:1419:10:
存储里有压缩原文，不得谎称没有更早历史
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 885 filtered out; finished in 0.11s
```

恢复实现后转绿。

独立复核者的证伪（`.recon/S11-verification.md` §9，7 个探针覆盖真继承前缀子会话 / 两轮 Full / 空页链 / rewind / 兜底分支 / 非前缀排除边界）：**旧实现下 5 个红，修复后 7/7 绿**；唯一打不穿的形状（非前缀排除）只能绕过 `full.rs` 造出，属分页设计层面的既有边界（见 §8）。

## 5. 复跑（原始命令）

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page      # 27 passed / 0 failed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib                      # 887 passed / 0 failed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib                      # 1817 passed / 0 failed / 6 ignored（交叉回归）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib                # 497 passed / 0 failed / 26 ignored
```

计数：`peri-acp --lib` 884（终局矩阵 5be107603 基线）→ 885（+审计探针）→ **887**（−探针 +3 回归用例）。

> `peri-resources` 冷开锁抖动：本节作者遇 1 次、独立复核者 6 跑 2 红（`connection_open_test.rs` 报 `session database initialization is busy`，来自 `connection.rs` 的 5s `SCHEMA_OPEN_LOCK_TIMEOUT`）。`peri-resources` 不依赖 `peri-acp`，相关文件也不在未提交改动里 ⇒ 预先存在的测试基建抖动，与本轮改动无因果关系（详见 `.recon/A-acceptance-report.md` §5.13 第 5 条与 `.recon/S11-verification.md` §1.3）。

## 6. 探针处置

1. wire 层探针 `zz_audit_initial_cursor_skips_archive_when_the_oldest_effective_own_row_is_system`（当时「通过 = 缺陷在」）→ **转正**为上表第一条回归用例（断言方向反转）。
2. 存储层探针 `peri-resources/tests/zz_audit_history_anchor_probe.rs`（3 条，未跟踪、非交付物）→ **删除**：它复刻的是 ACP 侧锚点判据，修复后会变成过期副本；同等覆盖由 ACP 侧真实 API 用例承担。
3. `peri-acp --lib` 计数：884（终局矩阵）→ 885（+审计探针）→ **886**（−探针 +2 回归用例）。

## 7. 文档同步

1. `docs/code-index/peri-acp.md`（`session/load` 模式分流一行）：锚点表述由「最旧**自有** payload」改为「最旧**可展示**自有 payload」，并写明判据与页装配同源。
2. 蓝图 `spec/issues/2026-10-13-transcript-shrink-history-pagination.md`：新增决策 **D15** 与切片 **S11**。
3. `.recon/A-acceptance-report.md`：A15 由 FAIL 升 PARTIAL（反例消失，剩余缺口是「不触发迁移/自动摘要」缺负向用例）；A20/A06 去掉 F1 相关缺口；§5.9 改写为修复记录。

## 8. 独立复核结论与边界

1. **结论（`.recon/S11-verification.md` §11）：该修复可以交付**——判据 1（无漏）/2（无重）/3（兜底不谎报）/4（判据同源）/6（同类残留）/7（边界不变）PASS；判据 5（用例非空转）PARTIAL：交付的用例「[System] → null」在旧实现上同样通过（兜底分支的*选择*当时无用例钉住）⇒ 已按其建议补 `the_fallback_anchor_keeps_an_archived_row_reachable`。
2. 复核者的第二条建议（注释写「最新一条**有效**自有 payload」）已采纳。复核期间的全部临时改动已 sha256 字节级还原，未做任何 git 写操作。
3. **本轮登记的既有边界（非本修复回退）**：页链是「严格早于锚点」的单向链；若将来出现**非前缀排除**（只归档一条中间消息并追加摘要），比锚点更晚的归档行会不可达。当前唯一写入者 `full.rs` 的排除选择规则 + rewind 清零规则保证形状不可达（`S11-verification.md` §2 的 I1 探针）；修复需另开切片（页链改成按位置区间浏览）。
4. **复核者的残余风险（与本修复无关）**：`peri-resources` 冷开锁 5s 抖动（见 §5）。

## 9. 边界（未覆盖）

1. 只到 ACP 层真实 API + 存储层；**没有**跑 TUI / SDK 端到端，也没有跑真实 stdio 客户端。
2. 客户端「页与首屏之间的非可展示行间隙」消费面未测（TUI 已有「空页 + 前进游标合法」的既有处理，本轮未新增端到端断言）。
3. 「锚点落在历史中间且更早方向仍有可展示行」的形态由页链自身覆盖，未另立客户端用例。
