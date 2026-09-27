# Standards 索引

本目录是工程规则的单一事实源；按任务读取，不默认整目录加载。

## 事实、规则与目标

- **现行行为**：由代码、契约测试和可复现结果证明；文档不能把尚未实现的目标写成现状。
- **工程规则**：`docs/standards/` 是可执行规则的单一事实源；根 `CLAUDE.md` 提供项目目标、设计取舍和路由，模块 `CLAUDE.md` 补充局部不变量与入口，不覆盖 standards。
- **变更目标**：由用户已确认的任务契约及已批准设计确定；active spec 记录实施范围与验收，history 仅提供背景。目标尚未实现不构成文档过时，已有测试也可能需要随获批契约变更而更新。
- **冲突裁决**：按 `STD-INDEX-002` 区分实现缺陷、文档过时与获批契约变更，不用一条信息优先级同时裁决现状和目标。

## 路由

| 任务 | 读取 |
| --- | --- |
| 跨模块边界、事件、Prompt、工具、中间件、安全 | [architecture-contracts.md](architecture-contracts.md) |
| Rust 实现 | [rust.md](rust.md) |
| `peri-tui` 界面与交互 | [tui.md](tui.md) 与 `peri-tui/CLAUDE.md` |
| `CLAUDE.md` 维护 | [documentation.md](documentation.md) |
| Git 分支创建、upstream、历史整理与 push 安全 | [git.md](git.md) |
| 测试（根 workspace、submodule、独立/side project 的范围与命令） | [testing.md](testing.md) |
| 权威设计与参考资料的生命周期 | [documentation.md](documentation.md) |

## 规则

### STD-INDEX-001

- **Scope**：所有工程任务。
- **Rule**：先按上表读取所需规则；测试规范只路由到 `docs/standards/testing.md`，不在本目录复制。
- **Verify**：`test -f docs/standards/testing.md && git diff --check`

### STD-INDEX-002

- **Scope**：规则与实现冲突。
- **Rule**：先核实当前行为及适用的已批准契约，再分类处理：实现违反契约时修复实现并补回归验证；文档过时且实现符合契约时修正文档；用户已批准改变契约时同步规则、设计、实现和测试。不得仅因实现或旧测试存在就降低目标要求。若目标与现行标准冲突，先核实本次授权是否包含该契约变更；无法从任务与批准记录判定时，只澄清影响目标或契约的分歧，不自行改写目标。
- **Verify**：核对当前行为证据、目标契约来源和冲突分类；检查同一变更中的实现、测试与文档保持一致，未实现目标仍明确标注状态。

### STD-SIZE-001

- **Scope**：仓库源码及测试文件（`.rs`、`.ts`、`.tsx`、`.mjs`、`.js`）；扫描遵循 `.gitignore` 并排除构建产物目录，文档预算另见 `documentation.md`。
- **Rule**：单个文件最多 1000 行，含空行和注释，源码与测试使用相同上限。新增或修改文件须满足限制；既有未改动超限文件的治理另立任务，不要求在无关任务中重构。拆分按职责和模块边界进行，不以机械搬移或压缩排版规避限制。
- **Verify**：`bash scripts/check-file-size.sh` 全量列出超限文件；对本次变更范围用 `wc -l <changed-source-paths>` 核对。全量扫描中的存量超限须如实报告，不宣称全库通过；自定义扫描阈值不替代此验收上限。
