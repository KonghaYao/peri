# rewind 切片独立复核（R1/R2）与处置

日期：2026-10-13。复核对象：worktree 内未提交的 rewind 相关改动（`peri-acp-types/src/store/history.rs`、
`peri-resources/.../sqlite_store/session_data/history.rs`、`peri-resources/.../remote/session_history.rs`、
`peri-acp/src/host/requests/rewind.rs`、`peri-acp/src/session/command/rewind.rs`、`peri-acp/src/dispatch/rewind.rs`）。
复核方式：独立 code-reviewer agent（read-only）+ 人工核对本文件中的每一条。

## 已判定为正确、不再改动的部分

1. **边界规则本身 PASS**：无 Compact / 落在排除段内 / 落在摘要上 / 两次以上 Full / System 夹在段内 /
   System 自身被排除 / 保留段中摘要存活 六类退化场景均有测试或明确分支。
2. **SQLite 原子性 PASS**：`BEGIN IMMEDIATE` 覆盖边界查找 + 删除 + 清零 + 派生刷新；清零行数不符整笔回滚
   （有测试断言行与 flags 逐项不变）。
3. **提交后再发布 PASS**：热态重建只在 durable 提交返回 Ok 之后发生；提交失败不发布、不改内存（有测试）。
4. **候选/预览/执行同源 PASS**：候选与预览读 `load_session_history`，执行读 `load_session_snapshot`，
   两者都是「继承区 ++ own、按 rowid 升序」，指纹不会永久失效（有 stale-preview 用例）。
5. **单一规则实现 PASS**：边界规则只有 `rewind_clear_excluded_ids` / `rewind_keep_len` 一份。

## 待修（按严重度）

| # | 严重度 | 位置 | 问题 | 最小修法 |
| --- | --- | --- | --- | --- |
| F1 | 真实缺陷 | `peri-resources/.../sqlite_store/session_data/history.rs:188-206`、`remote/session_history.rs:311-313`、`peri-acp/src/host/requests/rewind.rs:290-297` | 边界行在写入时已不存在 → 存储返回 `Ok(())` 且**什么都没做**（也不清零），但 ACP 仍重建并发布「已截断 + 已清零」的热态：存储与热态分叉，冷恢复与当前上下文不一致 | 让 `rewind_history` 返回「是否应用」；ACP 在「未应用」时报错且**不发布**。peri-agent 的 transcript 同步调用点保持「缺失即 no-op」语义 |
| F2 | 真实缺陷（远端） | `remote/session_history.rs:311` vs `:356` | 边界 rowid 在**批次之外**读出：并发追加若复用了 ≥ 边界的 rowid，会被这次 rewind 一并删除。批内守卫只覆盖清零集合，未覆盖删除下界 | 批内加一条守卫：断言边界行仍以同一 rowid 存在，否则整批中止 |
| F3 | 真实缺陷（远端） | `remote/session_history.rs:311-313` | 只读存储 + 未知边界时提前返回 `Ok`，早于 `ensure_writable`；SQLite 同类路径返回 `ReadOnlyStore` | 把可写性检查（或空效果批）提到边界读取之前 |
| F4 | 真实缺陷（前端一致性） | `peri-acp/src/session/command/rewind.rs:253-261` vs `host/requests/rewind.rs:294-296` | `RewindCompleted` 事件携带的是 **canonical** 保留前缀（含仍被存活摘要覆盖的正文），只有 wire 的 `history` 字段被改成有效视图。TUI 以该事件为事实源重建整个 transcript 与候选弹窗 → 用户看到模型/存储都不认为有效的条目 | 事件改用 `publish_rewind_view` 之后的**有效视图**（或把有效前缀传进 `execute_rewind`） |
| F5 | fail-open | `peri-acp/src/session/command/rewind.rs:224` | `if let (Some(store), Some(tid))` 在句柄缺失时**静默跳过** durable 提交，却照旧返回「已执行」并发事件 | 明确区分「无持久化句柄的合法场景」与「应报错」，默认显式失败 |
| F6 | 记录（不改） | `peri-acp/src/session/command/rewind.rs:122-134` | slash `/rewind` 走 `RemovedIds`：不重建有效视图、仍在内存里定位目标。当前无生产调用者（`dispatch::execute_command` 在本检出无调用方），wire 语义与 RPC 不同 | 仅在文档中标注；若恢复 slash 路由需走同一 plan |
| F7 | 记录（需裁决） | `peri-acp-types/src/store/history.rs:201` | 两次以上 Full 后回退到「前缀内部」位置时，`excluded` 连续段无法从 flags 判断批次边界 → 采「整段清零」：原文与其摘要同时有效（重复覆盖，不丢内容）。A19「无原文／摘要重复覆盖」在此窄场景未达 | 结构上无法从现有 flags 判定（`MessageFlags` 只有 `truncated/excluded/projection`，`projection` 是 Micro 投影指令，不含批次）。要精确修复需新增「覆盖批次」最小元数据 → 属 DDL，须用户批准；否则保留保守行为并记录 |

## 测试缺口（随 F1-F5 一并补）

1. 无「未知边界 ⇒ 存储不变 + 不发布热态」的用例（F1 正好落在这一空洞）。
2. 请求级用例的夹具没有继承区（规则文档要求调用方只交 own 历史，目前只有结构级单测兜底）。
3. 没有用例观察 `RewindCompleted` 事件载荷（F4 逃逸的正是这一点）。
4. `requests_rewind_cases_test.rs` 的夹具 `archive_prefix_in_storage` 手工拼装热态并丢掉刚追加的条目，
   起点状态在生产不可达。

## 处置顺序

F1-F5 与测试缺口合并为一次「rewind 修复」切片；因 S6c 会改 `host/requests/rewind.rs` 与
`session/command/rewind.rs`（结构版本递增点、回放失败语义），修复必须排在 S6 之后执行，避免写冲突。
F6 记录进蓝图；F7 作为待裁决项提单（需要 DDL 审批才能精确修复）。
