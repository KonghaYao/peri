# WorkState 重构：待合并交接

状态：已应用到原位置工作区，未 commit、未 push。按用户要求未重建。

## 目标与隔离

- 开发目录：`/Users/konghayao/code/ai/peri-workstate-redesign-20261006`。
- 目标目录：`/Users/konghayao/code/ai/peri-v4p3`，读取时分支 `pre-release/main`，HEAD `0c6d58d709af11596e9f5b9a8796d31424db7181`。
- 起始冻结基线：`b7c6770d2c0a85a4e7bd5ca3fd24e28e630a6ee8` 加原未提交改动；材料位于 `/tmp/peri-workstate-baseline-20261006-2iOGOn/`。
- 合并材料：`/tmp/peri-workstate-merge-20261006-5LbLRn/`，最终应用文件为 `ready-to-apply.patch`；`task-only.patch` 是相对冻结基线的原始增量，不应直接覆盖目标分支。

所有 Git 暂存与三方合并均在临时 `GIT_INDEX_FILE` 中进行，不改目标工作树或真实暂存区，不创建提交。最终 patch 以目标工作树快照为起点，不夹带最初复制进 worktree 的其它工作。

主 agent 在最终应用前执行 `git apply --check`，exit 0；补丁已应用到目标工作区，`git diff --check` exit 0。目标 HEAD 未改变；原有 `CLAUDE.md`、TUI issue 与未跟踪诊断材料均保留。材料包含 224 个文件的本次任务增量。schema 仍标 17，Native 按旧表形状区分新旧 schema17。未重建。

## 三方处理

- 目标已经前进两次提交，不能直接复制开发目录或使用整个 HEAD diff。
- 五个旧聚合测试/实现文件在目标分支被后续修改，但本重构明确退役它们：`work/processing_test.rs`、`terminal_query_test.rs`、`user_input_test.rs` 以及 Resources `work/effects.rs`、`effects_test.rs`。保留这些旧 API 文件会违反删除要求，因此在临时合并索引中显式删除。
- `peri-agent/src/session/exec/stage_builder.rs` 的唯一三方文本冲突是同一 retry context 修复的格式差异；保留目标已有版本，不重复覆盖。
- 目标额外的 TUI late-takeback 回归测试自动保留；目标诊断、运行错误处理及未提交的 `CLAUDE.md`、其它 issue 不回退。
- 临时合并快照的 Rust 中 `WorkState`、`WorkSnapshot`、`reduce_work`、`load_session_work` 与 `WorkPayload::from_payload` 残留为零；历史文档及一次性迁移中的旧表名不属于运行兼容实现。

## 应用结果

补丁已应用于 `/Users/konghayao/code/ai/peri-v4p3` 的 `pre-release/main` 工作区。目标分支没有新提交；本次改动与用户原有 WIP 均保持未提交、未暂存。未触碰真实数据库。

## 验证边界

Native 合成库 9/9、Remote 本地 transport 11/11 已通过；不是实际 Turso、完整 provider 或性能验收。旧聚合测试已迁移，最后发现的三处 Agent 字段断言及两处 Middleware 测试错误已修复。按用户要求，最后修复与合并快照不再重建，也未复跑完整测试。

用户确认 DB 尚未上线，因此当前和新 Work schema 均使用版本 17，不递增号码。Native 以 `session_work_state` 表形状区分旧/新 schema17：普通打开拒绝旧形状，显式停写迁移仍为 library API；真实旧库迁移需停全部 writer、核实备份，不自动复活旧活动执行。Turso 没有对应公开迁移 executor。真实 DB 没有被打开或迁移，资源效果由用户合并后实测。详见 [实施记录](2026-10-06-workstate-redesign-implementation.md) 与 [快速验收](2026-10-06-workstate-quick-acceptance.md)。
