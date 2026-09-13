# Workflow 描述修复与 grammar 试用

对应 `PERI-20260913-WORKFLOW-GRAMMAR`；issue 基线见 commit `73df17f7` 中的 `spec/issues/2026-09-13-workflow-script-description-mismatches-grammar.md`。现行事实源是 `peri-workflow/src/tool.rs`、`npm-packages/@peri-workflow/src/validate.ts` 和 `peri-workflow/src/journal/git.rs`。

工具描述已对齐 AsyncFunction body、唯一 meta export、顶层 primitives 与 return。示例明确传入 `writeIntent: {kind: "read_only"}`；只读意图对应 Git 事后检查，不能当作写工具权限隔离，也不覆盖 ignored 文件和仓库外写入。没有修改 parser、权限、执行或重试策略。

## 验证

- `cargo test -p peri-workflow --lib`：99 通过，3 个既有 E2E 跳过，0 失败。跳过用例标注需要安装 `@peri-code/workflow`，本轮未进行安装或真实 provider 执行。
- 在 `npm-packages/@peri-workflow` 执行 `bun test test/validate.test.ts`：12 通过；`bun run typecheck` 通过。
- 新增 Rust 回归用例直接从生产参数描述提取示例，调用随包 Node preflight，覆盖额外 export、import、缺 meta、五种旧式调用、缺 return，以及 cwd 不符时 Agent/registry/journal 副作用仍未发生。
- 独立审查提出的只读语义、旧式调用集合与测试规范问题已修正。

## 合成生成试用

使用同一 `gpt-5.6-luna`、high effort、无历史的独立 subagent，每版只读各自的工具说明和同一组 4 个任务，生成内联调用。先冻结 inputs 和 parser SHA-256，再运行；只调用 `node <artifact> validate <script.js> --json`，从未执行生成的 script、Agent 或 Workflow。每版允许一轮反馈修复，没有重复运行或独立模型对照，因此不作因果或生产效果估计。

| 观测 | 旧描述 | 候选描述 |
| --- | --- | --- |
| 首轮输出文件可按 JSON 解析 | 是 | 否：两处 JavaScript 单引号转义未正确编码到 JSON |
| 首轮 parser | 0/4：均缺 meta | 尚未运行：先被输出文件格式校验挡住 |
| 一轮修复 | 补 meta | 仅修 JSON 编码，脚本语义不变 |
| 修复后 parser | 4/4 通过 | 4/4 通过 |
| 修复后声明只读意图 | 4/4 | 4/4 |

原始输出、修复输出和 parser 结果分别保留。候选的无效原始 JSON 以 `outputs-b.invalid.txt` 保存，不能把修复后的文件当作首轮输出。这里的文件编码故障不是 Peri 原生 function calling 的实验，不能据此推断 provider 序列化存在同样问题。两版都发生过一轮修复，本轮没有证明总往返减少。

旧描述的修复输出仍把 return 放在嵌套函数中，parser 却未给警告；现行缺 return 检查只是一项宽松检查。由此把生产描述中“缺顶层 return 就会警告”的表述改为“检查是 advisory，不能证明会返回结果”。`candidate-tool.json` 保留试用时版本，`production-tool.json` 保存这次措辞校正后的版本；没有把旧输出归为新版本试用。代码回归仍从最终生产描述提取示例。

parser 通过只证明这里覆盖的 grammar 检查通过，不证明 primitives 参数、任务图、实际输出或仓库只读性成立。接下来若要比较任务收益，需固定真实执行环境、返回值与副作用验收，重复运行并独立评价；不能用本目录的 4 个任务估计总体成功率。

## 复核方法

按 `manifest.json` 核对输入与随包 `dist/peri-workflow.js` 指纹。将相应 `outputs-*.json` 的每条 `arguments.script` 原样写入临时 `.js` 文件，调用上述 Node validate 命令并记录最终 exit code、`ok/errors/warnings`。不要调用 `run`，也不要执行这些脚本。结果中的临时文件路径已省略，脚本 SHA-256 保留在各 `preflight-*.json`。
