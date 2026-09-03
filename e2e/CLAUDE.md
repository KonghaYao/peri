# E2E 测试开发指南

## Scope

本目录测试 Peri TUI 的真实终端交互（`tui-tester` + tmux）。Judge 消耗外部 API：**日常只跑目标文件；发版只跑分层门禁**（见下）。

## 怎么跑（唯一推荐路径）

在 `e2e/` 目录：

| 场景 | 命令 |
| --- | --- |
| 改单个用例 | `npm run e2e -- --file tests/<path>.test.ts --serial --retry 0` |
| PR / 日常冒烟 | `npm run e2e:l0` |
| 合并前 | `npm run e2e:l1` |
| **发版全量** | `npm run e2e:release` |
| 发版且不容忍首轮 flake | `npm run e2e:release:strict` |
| 交互选文件（调试） | `npm run e2e` |

门禁定义：`config/tiers.mjs`。结果看 `results/run-*/summary.json`（含 `flake.firstAttemptFailed`）。

**不要再用**：`npm run e2e -- --all`、`e2e:all`、发版时 28 个文件逐个 `--file` 手跑——已废弃，无 flake 统计且耗时长。

本地调试单文件（不走控制面报告）：`npm test -- tests/<path>.test.ts`（串行 vitest，无并行 worker 隔离）。

## 数据流

```text
run-e2e.mjs → vitest worker → helpers/peri.ts → dev.sh → Peri TUI (tmux)
            → summary.json / report.md / recordings/
```

`launchPeri()` 经 `dev.sh` 启动；隔离 `HOME` 时 cwd 为空临时目录，Read/Bash 应用 **`PROJECT_ROOT` 绝对路径**（见 `helpers/peri.ts`、`helpers/workflow.ts`）。

## 任务路由

| 任务 | 位置 |
| --- | --- |
| 启动、输入、稳定等待、抓屏 | `helpers/peri.ts` |
| Workflow 等待（磁盘 + 可选屏幕） | `helpers/workflow.ts` |
| LLM Judge | `helpers/judge.ts` |
| 录制 | `helpers/recorder.ts` |
| 控制面 / 分层门禁 | `scripts/run-e2e.mjs` + `config/tiers.mjs` |
| 场景用例 | `tests/**` |

## 稳定不变量

- 等待 UI 用**独有**文本，不匹配 prompt 回显或共享子串。
- `sendKey` 用 tmux 键名（`enter`、`escape`），不是字面字符。
- `waitForStableScreen(baseScreen)`：先变化再稳定（连续 3 次全文一致）。
- Judge 仅在测试里 `expect(result.pass)` 时作阻断；criteria 写可观察正向结果。
- 完成态优先 **磁盘因果**（如 workflow `state.json`），屏幕通知可能滚走。
- 不得把 `OPENAI_API_KEY` 写入测试、录制或报告。

## tmux / launchPeri

- pane 内 dev.sh 失败 → bash 提示符、无欢迎文本 → `launchPeri` 启动诊断应报「peri 启动失败」。
- 并行前控制面统一清理 `tui-test-*` session；worker 录制目录隔离（`E2E_RECORDINGS_DIR`）。
- 详见上文 Scope：Judge 与发版门禁分离。

## Verify

改完用例：`npm run e2e -- --file tests/... --serial --retry 0`。发版前：`npm run e2e:l0` 然后 `npm run e2e:release`。仓库根 `git diff --check`。
