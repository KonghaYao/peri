# E2E 测试开发指南

## Scope

本目录测试 Peri TUI 的真实终端交互。测试通过 `tui-tester` 驱动 tmux，按需使用 LLM Judge 评估屏幕。Judge 会消耗外部 API 资源：日常只运行目标测试文件；仅在发布前评估是否运行全量测试。

## 数据流/架构

```text
vitest → helpers/peri.ts → dev.sh → Peri TUI in tmux
       → ScreenCapture → recorder / Judge → test assertion
```

`launchPeri()` 以项目根目录为 cwd 通过 `dev.sh` 启动。`takePeriSnapshot()` 录制快照；`judge()` 只返回判断结果，是否阻断测试取决于测试是否断言 `result.pass`。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 启动、输入、稳定等待、抓屏 | `helpers/peri.ts` |
| LLM Judge 协议与结果解析 | `helpers/judge.ts` |
| 录制配置、快照和索引 | `helpers/recorder.ts` |
| HTML 报告 | `scripts/generate-report.ts` |
| 场景测试 | `tests/` 下对应目录与文件 |
| 终端驱动实现 | `tui-tester/` submodule |

## 稳定不变量

- 等待 UI 状态时使用该元素独有的文本，不能匹配 prompt 回显或其他共享文本。
- `sendKey` 使用 tmux 键名，如 `space`、`enter`、`escape`，不要把字面字符当作按键名。
- Judge criteria 以可观察的正向结果为主；不要用含糊的负向 UI 断言代替结果验证。
- AskUser 是 MessageArea 与 InputArea 之间的内联交互；按其题目导航与提交流程发送按键，不要按 overlay popup 假设处理。
- 调用 `waitForStableScreen()` 时传入提交 prompt 前的 `baseScreen`，先确认屏幕变化再判断稳定。
- recorder 默认写入 `.txt`；只有显式设置 `recorderConfig.ansi = true` 才额外写入 `.ansi`。Judge 使用内存中的 ANSI capture，不依赖录制文件。

## 目标命令

以下命令均从 `e2e/` 目录执行：

```bash
npm test -- tests/<目录>/<文件>.test.ts
npm run test:watch -- tests/<目录>/<文件>.test.ts
npm run report
```

发布前如需全量验证，执行 `npm test`。

## 按需引用 / Verify

- 新增场景前先阅读目标测试与对应 helper，复用 `launchPeri`、`sendPrompt`、`takePeriSnapshot`、`waitForStableScreen`。
- 使用 Judge 时，只在测试明确 `expect(result.pass)`（或等价断言）后将其结果作为阻断条件。
- 调试录制和报告分别使用 `recordings/` 与 `npm run report`；不要依赖 `.ansi` 默认存在。
- 修改完成后运行目标文件的测试，并从仓库根目录运行 `git diff --check`。不得把 `OPENAI_API_KEY` 或其他密钥写入测试、录制、日志或报告。
