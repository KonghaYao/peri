# ADLC 改进实施与验证

2026-09-13 启动，2026-09-14 收口。依据本目录的历史审计，逐项核对现行代码后实施。本文与冻结的历史证据分开；合成回归不代表真实任务的耗时、成本或成功率收益。

## 第一阶段：结构化结果

提交：`29e5b0dc`（`fix(workflow): validate integer results without rounding`）。

真实 `completed_result → validate_json_schema` 路径现在用原始数字文本判断整数：接受 `2`、`2.0`、`2e1`，拒绝 `2.5`、`2.0000000000000001` 和非零的极小负指数。递归检查 object/array 的 type、required、properties、items；`number` 继续接受整数及小数。这是明确的有限子集，未增加完整 JSON Schema、enum 或 minimum 支持。

新 fixture 在旧验证器上退出 101，复现合法 packet revision 被拒；最终提交快照在独立源码、独立 Cargo target 中执行 9 个结果投影测试，全部通过。提交 hook 的 check、clippy、fmt、typos 和 18 条分层依赖规则全部通过。早期共享构建目录曾产生与快照源码不一致的类型错误，未用该失败或共享缓存结果作为最终通过证据。

## 第二阶段：恢复、边界和阶段证据

实施与回归已完成。第二阶段提交包含本节对应代码、内置 Skill 和可重放实验；提交身份可通过本文件的 Git 历史核对。

| 候选 | 已落实的行为 | 验证入口 |
| --- | --- | --- |
| 01 整数协议 | 精确数字原文判断，不靠把 schema 改宽 | `peri-agent/src/agent/workflow/agent/result_test.rs` |
| 02 边界成本 | 内嵌 CLI 的有界扫描；可信基线摘要先验校验；明确授权且无 tracked 文件的生成目录只核对身份 | `npm-packages/@peri-workflow/test/boundary.test.ts`；本目录 `boundary-replay.mjs` |
| 03 恢复范围 | 保存/恢复 args 与并发数；坏恢复源报错；只缓存连续成功前缀；按依赖和四类 blocker 生成恢复建议 | Rust journal/runner/lifecycle tests；Node server/adlc tests |
| 04 包预算与交接 | Skill 按可验证依赖安排物理运行；里程碑保存部分交接；source compile/check 未通过时 sink 不启动 | Node planner 的 partial/source gate fixture；`ultra-adlc/SKILL.md` |
| 05 dirty 与责任 | 未变 dirty 保留；同状态内容修改、ignored 越界与生成根替换可见；变更责任仍可未知 | boundary 的临时 Git fixture |
| 06 阶段产物 | 必需文件、SHA256 和本轮生产者身份检查；合法 generic/cache 零新调用可用；不修改通用引擎的 unknown/完成语义 | Node `checkAdlcStage` fixtures |

`peri workflow` 在配置/Agent 初始化前调用当前二进制内嵌的 Node artifact，避免模型引用仓库内无法随二进制取得的脚本，也不需要临时下载 npm 包。CLI 支持 `boundary snapshot/compare` 和 `adlc check-stage/plan`。方法和请求示例由内置 [ultra-adlc skill](../../../../peri-middlewares/src/skills/builtin/skills/ultra-adlc/SKILL.md) 维护；代码导航见 [Workflow 索引](../../../../docs/code-index/peri-workflow.md)。

## 验证记录

以下验证使用只包含本任务改动和已提交基线的独立源码快照及独立 Cargo target。没有将共享工作树中其他任务的未提交代码混入验证。

| 检查 | 结果 |
| --- | --- |
| `cargo test --offline -p peri-workflow --lib` | 108 通过，3 个原有 ignored 测试未执行 |
| `cargo test --offline -p peri-middlewares --lib workflow` | 19 通过，包含真实 Node 的取消、恢复参数与并发上限回归 |
| `cargo test --offline -p peri-middlewares --lib ultra_adlc` | 6 通过，验证 Peri 实际内置 Skill 注册和契约 |
| `cargo test --offline -p peri-tui --bin peri cli_workflow` | 2 通过，覆盖全局参数、歧义输入和 print 冲突 |
| `cargo test --offline -p peri-workflow --doc` | 命令通过；当前没有 doc test 用例 |
| `cargo build --offline -p peri-tui --bin peri` | 构建通过 |
| `bun run build` / `bun run typecheck` / `bun test` | 构建及类型检查通过，106 个测试通过；隔离目录重建 bundle 与提交字节一致 |
| 实际 `peri workflow` CLI | 13 个场景通过：坏配置下的帮助、全局参数、错误命令、脚本验证、评估身份/缺产物、文件快照及越界退出码 |

跨语言验证曾发现 Rust 普通启动发送 `resume: null` 被新增校验误拒，现已修复并通过真实 dist JSON-RPC 回归。指定恢复来源时缺失/null 日志仍拒绝。另一条旧 CLI 测试依赖开发机已有历史目录，现已改用自建 fixture。没有把这些失败归因于模型或通过重试隐藏。

标准 Codex Skill 校验器不识别 Peri 已有的 `argumentHint` / `userInvocable` frontmatter，未把该结果记作通过，也未删除合法 Peri 字段；这里使用实际 Peri loader 的测试作为兼容依据。

## 可重放边界实验

运行 `node side-projects/agent-defect-analyzer/reports/2026-09-13-adlc-bottlenecks/boundary-replay.mjs` 可重建独立临时 HOME 与 Git 仓库。脚本记录扫描计数、扫描耗时、CLI 墙钟时间和 artifact SHA256，完成后清理 fixture。保存的原始结果见 [boundary-replay.json](boundary-replay.json)。

| 生成目录内文件数 | 实际读取文件 | 实际读取字节 | snapshot 扫描时间 | snapshot CLI 墙钟 |
| --- | --- | --- | --- | --- |
| 0 | 5 | 122 | 17 ms | 50 ms |
| 10,000 | 5 | 122 | 18 ms | 52 ms |

两组均通过未变 dirty、allowlist 修改检查；同长度 ignored 越界内容被检出；缺失或错配基线均非零退出。这是同一小型合成 fixture 的单次端点测量，只证明已授权生成目录内容不会放大读取量，不是统计性延迟结果，也不支持真实任务提速倍数。阶段产物检查另有 5 × 16 MiB 稀疏文件回归：总预算 64 MiB 用尽后，第五个文件不再读取。

## 解释边界

- 阶段文件 gate 验证文件完整性与 Main 提供的 host 身份事实，不评价文件内容是否正确；产品完成仍依赖独立评估及真实验收证据。
- Planner 的成功表示形成有效恢复建议，pending 工作不会因此完成。它不授予权限，也不自动启动 Agent。
- 文件快照是有范围的端点观察，不是操作系统沙箱、原子文件系统快照或写入者归因。仓库外写入、两次捕获之间写后恢复，以及声明生成目录的内部内容不在其证明范围内。
- 未实现内容 hash 缓存；性能改善来自明确生成目录策略与工作量上限。扫描超限、覆盖不足、身份错配仍失败，不自动跳过目录。
- 总预算仍由 Skill 的逻辑账本和物理运行分配管理；引擎只强制单次运行上限。没有补造失败尝试的未知 token 消耗，也没有实现新的跨运行计费协议。
- 未执行新的付费模型任务或全量历史任务 A/B。真实任务恢复次数、成功率与总成本需要后续同口径观察。

后续观察应继续区分产品缺口、能力、协议与收尾四类原因，并记录依赖修订、复用证据、真实新启动尝试和最终交付；不能只看物理 run 数变少。
