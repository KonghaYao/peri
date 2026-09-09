# P0：Micro Compact 截断 Edit/Write 历史参数并向模型注入伪源码

**状态**：Resolved（implemented and independently verified）
**优先级**：P0
**类型**：数据完整性 / Agent 执行安全 / 上下文投影 / 工具系统
**创建日期**：2026-09-09

## 事故摘要

历史 Micro Compact 会把旧 tool call 的长字符串 input 改写为 head/tail 投影视图，并插入 Peri V1 sentinel：

```text
... [N 字符已省略] ...
```

模型可能复制该伪源码到新的 `Write.content` 或 `Edit.old_string` / `Edit.new_string`，随后由内建文件工具写入磁盘。Micro 不会在同一 tool call dispatch 前直接改写当前执行参数；风险链路是“历史 provider-facing projection → 模型复制 → 新 call → sink”。

## 已接受修复范围

决策事实源：`.peri/adlc/tasks/2026-09-09-micro-compact-tool-input-p0/decisions/D-001.md`。

1. Micro planner 不再生成 tool-input action；renderer 对当前可解码 legacy directive 采取防御性 Preserve，provider-facing `ToolCallRequest.arguments` 与 `ContentBlock::ToolUse.input` 保持 canonical transcript 原值。
2. persisted directive restore 使用 `Absent` / `Valid` / `Invalid`：仅 `Absent` 可重新规划；`Valid` 只恢复安全、独立且成功的 `ToolResult` action；`Invalid` 使用 canonical view且本轮不 replan。
3. producer 与 sink 共用 V1 sentinel grammar。内建 `Write` / `Edit` 在 per-target critical section 内检查完整 post-image 相对 pre-image 的 sentinel multiset delta；新增 sentinel 时在文件系统副作用前拒绝。
4. Write overwrite/append/draft 与 Edit single/replace-all 使用同一 full-post-image transaction。Draft 仅在成功提交后消费 exact id。
5. Full Compact 不改算法；只验证摘要输入直接来自 `transcript.visible_messages()`，不消费 Micro renderer view。
6. HITL 不重构；只验证最终 edited input 继续经过相同内建 Write/Edit sink。

## 稳定不变量

稳定跨层契约归位于 `docs/standards/architecture-contracts.md` 的 `ARC-MICRO-TOOL-INPUT-001`：

- Micro 不得修改任何历史 tool input。
- V1 sentinel 不得相对 pre-image 作为新文件内容经内建 Write/Edit 引入。
- 历史已有 sentinel 不扫描、不迁移、不修复；未新增时允许无关修改。

## Verification checklist

- [x] shared formatter/scanner 覆盖 LF、CRLF、EOF、ASCII digits、超长数字与任意 UTF-8。
- [x] planner 不生成 `CompactToolInput`；estimator 对 input/no-op action 计零收益。
- [x] 当前可解码 legacy input directive 不继续污染 provider-facing view；非法 ToolCall/ToolUse action保持原文。
- [x] Tool-result Micro projection 继续工作。
- [x] 内建 Write/Edit 对 overwrite、append 跨边界、Edit 拼接、replace-all、draft restore 做完整 post-image delta guard，并验证拒绝无副作用。
- [x] fake-model sentinel copy 链在合理 seam 上由 provider-view 保真回归与内建 Write/Edit sink 回归共同锁定。
- [x] Full fake model 捕获输入，证明来自 canonical transcript；未改变摘要算法。
- [x] HITL edit 回归确认 `ApprovalDecision::Edit.new_input` 成为后续调用输入；该输入仍由同一 Write/Edit sink 执行与校验。
- [x] WP-005 目标测试、lint 与 `git diff --check` 最终通过。

## 最终裁决

- Round 2 独立代码复审：`ACCEPT`，无剩余 P0/P1。
- 最终独立 assessor：`ACCEPT`。
- Sentinel contract：8 passed；`compact_v2`：159 passed；filesystem targeted：202 passed。
- `peri-middlewares --lib -- --test-threads=1`：1611 passed / 0 failed / 4 ignored。
- Workspace build、all-target clippy、workspace doc tests 与 `git diff --check`：全部通过。
- 默认并行 middleware suite 的 23 个失败已在 clean HEAD 等量复现并登记为既有 baseline，不属于本修复。
- 未扫描、迁移或修复任何历史文件、session 或 transcript。

## 明确 non-goals

- 不扫描、迁移、删除或修复历史文件、session、transcript；不推断具体历史文件状态。
- 不增加 unknown/corrupt persisted store 或未来 wire format 的恢复能力。
- 不重构 Full pair-closed、摘要算法、失败恢复或 compact liveness。
- 不重构 HITL binding / approval 状态机；本次只确认最终 edited input 经过同一 sink。
- 不覆盖 Bash、外部进程、任意 MCP server、SandboxWrite 或 PTC direct Node/OS API。
- 不重新启用任何 tool-input projection；长期 invocation identity/字段语义化 policy 另行设计。

## 目标验证

```bash
cargo test -p peri-acp-types --lib sentinel
cargo test -p peri-agent --lib compact_v2
cargo test -p peri-agent --lib reason
cargo test -p peri-middlewares --lib -- tools::filesystem
cargo test -p peri-middlewares --lib permission
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```
