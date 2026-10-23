# P1：子 Agent 内存驻留与重复正文

状态：实现完成，待 CI。基线 `68971133c`（origin/main），工作树 `subagent-memory-retention`。日期：2026-10-23。

## 故障与证据

报告基于代码与运行时链路复核，M01–M03、M06–M08 为内存驻留/记账缺陷（不改变业务语义）：

1. **M01 未使用的父历史物化**：`peri-middlewares/src/subagent/tool/define.rs` 在 fork/后台分流前无条件 `current_messages`；named 子 Agent 的 sync/后台路径不消费，错误路径也先付整份深复制。`invocation.rs` 的 `BaseMessage` clone 是深复制，不是 Arc 共享。
2. **M02 resume 预检读正文**：`peri-agent/src/session/subagent/factory/resume.rs` 用 `load_active_snapshot` 只取 `.binding`；生产实现会读 payloads/inherited。claim 返回的 meta 已覆盖后续字段，预检快照的正文部分冗余。
3. **M03 fork 父历史长期存活**：`peri-agent/src/session/subagent/factory/spawn.rs` 对 `parent_messages` 逐条 clone；变量存活到函数末尾，与 `build_subagent_session_v2` 装配期重叠。
4. **M06 归档登记绕过**：`turn.rs` 的 stale/cancel/suspend 三条路径直接 `committed.push_back(view_models)`，未走 `register_live_seal_segments`，窗口预算低估。
5. **M07 诊断不可信**：`memory_probe.rs` 的 `unique_*` 用枚举槽地址去重，同一 `Arc` 出现在不同槽位会重复计入。
6. **M08 fingerprint 常驻**：`bg_complete.rs` 用完整 reminder JSON 字符串做去重 key，成功 bg 任务的输出正文随 Accepted 条目长期驻留。

## 本轮范围

- 修复 M01、M02、M03、M06、M08。
- M07：修复 `unique_*` 的分配身份去重，并补齐 raw 累积器、BG live 详情的分层计量（含 `parent_tool_call_id`）。
- M04/M05 的安全子集：仅当详情正文存在第二持有者时收缩终态 BG 展示单元；释放后以墓碑保留身份/终态/丢弃计数；`shell_detail` 面板改为借用读，避免每帧全量 clone。完整释放/回读边界仍延后。
- 新增回归测试覆盖：resume 预检不读快照、挂起/stale 归档记账、跨层 Arc 去重、指纹定长且内容/类别敏感、BG 终态收缩仅释放重复正文。
- 测试替身 `load_session_binding` 改为返回真实 region binding（含父会话分类），并新增 `snapshot_reads` 观察点。

## 明确延后

- **M04/M05 完整生命周期**：需要持久 execution 身份、occurrence 起止锚点、子线程 committed tail 与按 `child_thread_id` 回读通道；当前协议不具备，按预算释放唯一正文会丢数据。本轮只做“存在第二持有者”前提下的收缩。
- **M05 的跨会话回读**：BG 详情墓碑只保留身份/终态，不保存正文；需要回读时另立协议扩展。
- **M07 的更细粒度探针**：本轮覆盖累积器与 BG 分层；逐字段覆盖率扩展另立任务。
- **既有测试隔离问题**：`peri-tui` 并行全量有 2 个与本改动无关的失败（client session request、plugin guard），`peri-middlewares` 有 2 个失败；均已在 `origin/main` 基线复现，另立 issue 处理。

## 验收

- [x] M01：named sync/bg 不物化父消息；fork 行为不变（既有 fork wire 测试通过）。
- [x] M02：预检只读 binding；`snapshot_reads == 1`；claim 后历史失败仍回滚。
- [x] M03：fork inherited id 顺序与 `snapshot_at_message_id` 不变（既有 fork 测试通过）。
- [x] M06：stale/cancel/suspend 均登记窗口预算；冻结语义不变。
- [x] M07：同一 Arc 跨层 unique 只计一次；独立分配仍分别计入；raw/BG 分层可区分。
- [x] M08：幂等/冲突语义不变；去重 key 固定 64 字符十六进制摘要，且包含任务类别。
- [x] `peri-agent`、`peri-middlewares`、`peri-tui` 相关 lib 测试通过（基线既有失败除外）。
- [ ] CI 与 CodeRabbit 终态。

## 验证命令

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
```
