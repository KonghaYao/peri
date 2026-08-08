# E2E 测试开发指南

## Scope

本目录测试 Peri TUI 的真实终端交互。测试通过 `tui-tester` 驱动 tmux，按需使用 LLM Judge 评估屏幕。Judge 会消耗外部 API 资源：日常只运行目标测试文件；仅在发布前评估是否运行全量测试。

## 数据流/架构

```text
run-e2e.mjs(控制面) → vitest 子进程 → helpers/peri.ts → dev.sh → Peri TUI in tmux
       → ScreenCapture → recorder / Judge → test assertion
```

`launchPeri()` 以项目根目录为 cwd 通过 `dev.sh` 启动。`takePeriSnapshot()` 录制快照；`judge()` 只返回判断结果，是否阻断测试取决于测试是否断言 `result.pass`。

## 控制面（并行执行）

日常运行推荐走 `scripts/run-e2e.mjs`：可选择用例、多进程并行执行（默认 3 worker）、失败自动重试、收集结果并生成报告（终端汇总 + Markdown + summary.json，含失败点与 Judge 明细）。

```bash
npm run e2e                                # 交互式选择用例
npm run e2e -- --all                       # 全部用例并行
npm run e2e -- --only rewind               # 文件名子串过滤
npm run e2e -- --only 'scenarios/*.test.ts'
npm run e2e -- --dir tool-cards            # 按 tests/ 下目录过滤
npm run e2e -- --file tests/smoke/basic-question.test.ts
npm run e2e -- --parallel 3 --retry 1 --verbose
```

- 每个 worker 是独立 vitest 进程，录制目录隔离（`results/run-<ts>/recordings/worker-<i>/`），通过 `E2E_RECORDINGS_DIR` / `E2E_PARALLEL` 环境变量生效；残留 tmux session 由控制面启动前统一清理。
- 输出：`results/run-<ts>/report.md`（Markdown）+ `summary.json`（机器可读，含失败测试、断言位置、Judge 失败项明细）。
- 直接 `npm test` 仍是串行（保守模式，不设置上述环境变量）。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 启动、输入、稳定等待、抓屏 | `helpers/peri.ts` |
| LLM Judge 协议与结果解析 | `helpers/judge.ts` |
| 录制配置、快照和索引 | `helpers/recorder.ts` |
| HTML 报告 | `scripts/generate-report.ts` |
| 并行控制面、结果汇总 | `scripts/run-e2e.mjs` |
| 场景测试 | `tests/` 下对应目录与文件 |
| 终端驱动实现 | `tui-tester/` submodule |

## 稳定不变量

- 等待 UI 状态时使用该元素独有的文本，不能匹配 prompt 回显或其他共享文本。
- `sendKey` 使用 tmux 键名，如 `space`、`enter`、`escape`，不要把字面字符当作按键名。
- Judge criteria 以可观察的正向结果为主；不要用含糊的负向 UI 断言代替结果验证。
- AskUser 是 MessageArea 与 InputArea 之间的内联交互；按其题目导航与提交流程发送按键，不要按 overlay popup 假设处理。
- 调用 `waitForStableScreen()` 时传入提交 prompt 前的 `baseScreen`，先确认屏幕变化再判断稳定。稳定判定基于完整文本内容（连续 3 次一致），不是屏幕长度。
- `launchPeri` 内置启动诊断：欢迎文本 30s 内未出现时轮询检测失败特征（shell 提示符/cargo error/session 消失），快速抛"peri 启动失败"而非静默继续；cargo 慢编译最多再等 60s。tmux session 创建失败自动重试 3 次。
- Judge 响应无效（JSON 解析或结构校验失败）会自动带反馈重试一次；两次都无效才判失败。Judge 的 system prompt 要求"检查清单是唯一判断依据"——criteria 中列出的可接受值集合/格式定义会被严格采信。
- recorder 默认写入 `.txt`；只有显式设置 `recorderConfig.ansi = true` 才额外写入 `.ansi`。Judge 使用内存中的 ANSI capture，不依赖录制文件。

## tmux 生命周期（排查 "tmux 挂" 用）

tmux 默认行为决定了两条关键链路：

1. **pane 进程退出 → window 关闭 → session 自动销毁（`remain-on-exit` off）**；最后一个 session 销毁 → **server 退出（`exit-empty` on）**。
2. 测试的 pane 是**交互式 bash**：dev.sh 失败退出后 bash 返回提示符、session 存活但欢迎文本永不出现。

因此"tmux 时不时挂"（快速失败报 `can't find pane` / `no server running`）的根因通常是 **dev.sh 启动失败或提前退出**（cargo 编译失败、环境问题），而非 tmux 本身崩溃。`launchPeri` 的启动诊断会将其明确为 "peri 启动失败" 并附屏幕片段。

维护要点：
- 不要依赖"tmux server 常驻"：测试间 server 会因 exit-empty 反复退出/重建，`new-session` 偶发失败是正常的（`startTester` 已重试）。
- 测试进程被 SIGKILL（控制面 worker 超时）会残留挂起的 dev.sh session；控制面优先 SIGTERM 优雅终止，启动前统一清理残留。
- tmux 命令（exec）一律带超时，异常挂起不能卡住测试进程。

## 目标命令

以下命令均从 `e2e/` 目录执行：

```bash
npm run e2e -- --only tests/<目录>/<文件>.test.ts   # 控制面（并行 + 报告）
npm test -- tests/<目录>/<文件>.test.ts             # 单文件（串行）
npm run test:watch -- tests/<目录>/<文件>.test.ts
npm run report
```

发布前如需全量验证，执行 `npm run e2e -- --all`（并行）或 `npm test`（串行）。

运行前置：`tests/setup.ts` 会在测试启动前自动清理残留 `tui-test-*` tmux session（防止上次运行残留干扰 tmux server）；tmux 未安装或 server 不存在时静默跳过。并行模式下清理由控制面统一执行，worker 间互不干扰。

## 按需引用 / Verify

- 新增场景前先阅读目标测试与对应 helper，复用 `launchPeri`、`sendPrompt`、`takePeriSnapshot`、`waitForStableScreen`。
- 使用 Judge 时，只在测试明确 `expect(result.pass)`（或等价断言）后将其结果作为阻断条件。
- 调试录制和报告分别使用 `recordings/` 与 `npm run report`；不要依赖 `.ansi` 默认存在。
- 修改完成后运行目标文件的测试，并从仓库根目录运行 `git diff --check`。不得把 `OPENAI_API_KEY` 或其他密钥写入测试、录制、日志或报告。
