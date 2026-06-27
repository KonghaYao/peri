# v2 重做验证步骤

> 每个 Stage 完成后必跑。clippy/fmt 任何 warning 都不能放过。

## 标准验证套件（每个 Stage 完成后）

```bash
# 1. 编译
cargo build --workspace

# 2. v1 路径全量测试
cargo test --workspace --lib

# 3. v2 路径全量测试（关键！）
PERI_USE_V2=1 cargo test --workspace --lib

# 4. clippy 严格（all-targets 含测试 / bench / bin）
cargo clippy --workspace --all-targets -- -D warnings

# 5. 格式化
cargo fmt --all -- --check
```

### 当前基线（2026-06-25 Stage 1 完成）
- 测试：**2912 passed / 0 failed / 4 ignored**（v1 + v2 等价）
- build：绿
- clippy：零 warning
- fmt：零 diff

### 已知 ignored（4 个）
- `peri-workflow::runner::tests::test_e2e_simple_workflow`（1 个）—— 需要 `@peri-code/workflow` 已安装
- 其他 3 个 —— 见各 crate 的 `#[ignore]` 标注（搜索 `#[ignore]`）

---

## 手动 smoke test（切默认前必跑）

```bash
# v2 路径
PERI_USE_V2=1 cargo run -p peri-tui -- -a
```

**测试矩阵**（每项跑 5 轮，记录任何异常）：

### 对话 + 工具调用
1. 输入「读取 README.md 并总结」—— 验证 Read 工具 + 文本回答
2. 输入「在 /tmp 写一个 hello.txt」—— 验证 Write 工具 + HITL 审批
3. 输入「grep 'v2' 在 docs/」—— 验证 Grep 工具

### Compact
1. 持续对话到上下文 > 70% —— 验证 Micro compact 触发 + UI 提示
2. 持续对话到上下文 > 85% —— 验证 Full compact 触发 + 摘要注入
3. 设置 `DISABLE_COMPACT=1` 跑同样场景 —— 验证 compact 不触发（Stage 1 Top 4 修复点）

### Cancel
1. 触发长 LLM 调用（如复杂问题）后按 Ctrl+C —— 验证 v1 路径 cancel 响应
2. 触发 Full Compact（上下文 > 85%）后立即 Ctrl+C —— 验证 v2 compact cancel 响应（Stage 1 Top 8 修复点）

### SubAgent（双轨边界）
1. 输入「派出 agent 查找 X 文件」—— 验证 SubAgent fork 走 v1 路径不崩
2. 输入「后台执行 Y」—— 验证 bg agent 走 v1 路径，结果回传到主对话

### DTO 渲染
1. TodoWrite 工具调用 —— 验证 TodoItemDto 正确渲染（Stage P4.2）
2. 触发 compact —— 验证 CompactFileInfoDto 渲染（Stage P4.1）
3. Workflow 进度（若可用）—— 验证 WorkflowProgressDto 渲染

---

## 验证 agent 派遣模板

复杂修复（>3 文件 / 架构变更）建议派遣 verification agent：

```
独立验证 [修复描述]。原始任务：[简述]。
修改的文件：[列表]
修改方案：[每处改动的 why]
验证步骤：
1. 读取修改后的文件，确认改动落地
2. 跑 cargo build --workspace
3. 跑 cargo test --workspace --lib
4. 跑 PERI_USE_V2=1 cargo test --workspace --lib
5. 跑 cargo clippy --workspace --all-targets -- -D warnings
6. 跑 cargo fmt --all -- --check
关键对抗点（请独立判定）：
- [列出可能的副作用 / 不变量破坏]
报告 PASS / FAIL / PARTIAL，列出证据。
```

---

## 切默认前置检查（Stage 2/3 完成后）

```bash
# 1. 跑全部 v2 测试 1 周（每日）
PERI_USE_V2=1 cargo test --workspace --lib

# 2. 跑全部 v2 手动 smoke test 矩阵（每项 5 轮）

# 3. 跑 Langfuse trace 对比（v1 vs v2 同 prompt）
# 验证：事件序列、step 关联、字段完整度（Stage 3 修复后）

# 4. 检查无回归
git log --oneline -20
cargo test --workspace --lib | grep -E "FAIL|panic"
```

切默认后保持 `PERI_USE_V1=1` 回退路径可用至少 2 周。

---

## 故障排查

### 编译错误
- `cannot find type ReActAgentParts` → 检查 `peri-agent/src/agent/executor/mod.rs::into_parts` 是否被破坏
- `trait Middleware<S>` 还存在 → 检查是否漏改某个 `impl<S: State>`
- `AgentEvent not found` → 检查是否误删 v1 `events.rs`（应在 P5.5 才删）

### 测试失败
- v1 测试失败 → 检查是否破坏了 `executor/mod.rs` 路径
- v2 测试失败 → 优先检查 StageContext 字段完整性
- SubAgent 测试失败 → 检查是否漏改 `SubAgentMiddleware`（仍走 v1）

### 工作流故障（"0 agents, 0 tool calls"）
按 CLAUDE.md「Workflow 故障排查」段落：
1. `which peri-workflow` 存在
2. `cargo build -p peri-workflow -p peri-acp` 通过
3. 重启 Peri TUI

---

## grep 速查

```bash
# v1 残留检查（P5 完成后应为零）
grep -r 'ReActAgent' --include='*.rs' .
grep -r 'trait Middleware<S' --include='*.rs' .
grep -r 'use peri_agent::agent::events::AgentEvent' --include='*.rs' .

# 双轨边界检查
grep -r 'executor\.execute()' --include='*.rs' .
grep -r 'build_and_execute_agent_v2\|build_and_execute_agent' --include='*.rs' .

# Stage 1 修复验证
grep -n 'sink_for_v2' peri-acp/src/session/executor.rs  # 应只在注释中出现
grep -n 'with_compact_llm' peri-acp/src/agent/builder_v2.rs  # 应有一处
grep -n 'DISABLE_COMPACT' peri-agent/src/agent/stages/compact.rs  # 应有
```
