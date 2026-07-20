# E2E 测试开发指南

## 铁律

**永远不要一次跑全部测试。** 每次 Judge 调用消耗 LLM API token，全量 4 个测试约 207s + ~11 次 API 调用。
- 日常开发：`npx vitest run tests/<目录>/<文件>.test.ts`
- 修复验证：只跑被修改的测试
- 仅在发布前跑全量

## 测试架构速览

| 目录 | 用途 |
|------|------|
| `tui-tester/` | git submodule，终端测试库（TmuxTester + SnapshotManager） |
| `helpers/peri.ts` | `launchPeri()` / `sendPrompt()` / `takePeriSnapshot()` / `waitForStableScreen()` |
| `helpers/judge.ts` | `judge({ansiRaw, criteria})` → `JudgeResult {pass, checks[]}` |
| `helpers/recorder.ts` | `.txt` / `.ansi` 写入 + `index.jsonl` 录制 |
| `scripts/generate-report.ts` | 单文件 HTML 报告（unpkg ansi_up CDN） |
| `tests/` | vitest 测试文件 |

## 常见陷阱

### 1. `waitForText` 匹配了错误的文本

**问题**：`waitForText("AskUserQuestion")` 匹配了用户 prompt 回显（`❯ 请你测试一下 AskUserQuestion 工具...`），而非面板标题。

**正确做法**：用 UI 元素独有的文本。AskUser 面板标题是 `"Ask User"`（不是 `"AskUserQuestion"`）。

```ts
// ❌ 用户 prompt 就含这个文本，会过早匹配
await tester.waitForText("AskUserQuestion", { timeout: 30_000 });

// ✅ 面板标题，只有面板打开时才出现
await tester.waitForText("Ask User", { timeout: 60_000, interval: 500 });
```

### 2. `sendKey` 键名 vs 字面字符

**问题**：`sendKey(" ")` 在 tmux 中发送字面空格字符，不被 crossterm 识别为 Space 键。

**正确做法**：用 tmux 键名字符串。

```ts
// ❌ 字面空格，不会触发面板的 Space 选中
await tester.sendKey(" ");

// ✅ tmux 键名
await tester.sendKey("space");
```

tui-tester 的 `parseTmuxKey` 将字符串映射到 tmux 键名，支持：
`"enter"`, `"tab"`, `"escape"`, `"space"`, `"up"`, `"down"`, `"left"`, `"right"`, `"backspace"`, `"delete"`, `"home"`, `"end"`, `"pageup"`, `"pagedown"`, `"f1"`-`"f12"`

### 3. Judge criteria：正向断言优于负向断言

**问题**：负向断言（"不应有 X"）容易误判。例如 `"不应出现 ○/● 标记"` 会把工具卡片的 `● AskUserQuestion` 标题误判为未关闭的面板选项。

**正确做法**：验证应该出现的内容，而非验证不应该出现的内容。

```ts
// ❌ 负向断言——Judge 分不清工具卡片 ● 和面板选项 ○
"屏幕上不应再出现 ○/● 选项标记"

// ✅ 正向断言——验证 agent 的行为结果
"agent 应已完成了对 AskUserQuestion 工具的测试，输出了总结（如包含表格）"
"消息区应包含 agent 对用户回答内容的引用，表明 agent 确实收到了选择"
```

### 4. AskUser 面板是内联 Panel，不是 Popup

**问题**：以为 AskUserQuestion 是弹窗（popup overlay），实际是内联 Panel，夹在 MessageArea 和 InputArea 之间。

**交互流程**：
- `Space`：选中/取消当前选项
- `Enter`：下一题（最后一题 Enter 提交全部）
- `Tab`：切换问题
- 多道题需要多次 Space+Enter

```ts
// 3 道题的完整交互
for (let q = 0; q < 3; q++) {
  await tester.sendKey("space");  // 选中
  await tester.sleep(150);
  await tester.sendKey("Enter");  // 下一题/提交
  await tester.sleep(300);
}
```

### 5. `waitForStableScreen` 的 baseScreen 参数

**问题**：不传 `baseScreen` 时，可能在 thinking 阶段就认定屏幕稳定并退出。

**正确做法**：总是传入 prompt 提交前的屏幕做基准。

```ts
const base = await tester.getScreenText();  // prompt 提交前
await sendPrompt(tester, "...");
await waitForStableScreen(tester, 120_000, base);  // 先等变化再等稳定
```

### 6. recorderConfig 默认只写 .txt

`recorderConfig.ansi` 默认 `false`——只生成纯文本 `.txt`（人类可读），不生成 `.ansi`（含 escape codes）。

HTML 报告和 Judge 不依赖 `.ansi` 文件：报告有 `.txt` → `.ansi` 回退逻辑，Judge 用内存中的 `capture.raw`。

如需彩色录制：在测试中设置 `recorderConfig.ansi = true`。

## 调试技巧

### 查看单步截图

```bash
cat e2e/recordings/<name>.txt
```

### 查看 Judge 日志

测试 stdout 会打印 Judge 结果（含每个 criterion 的 pass/fail + detail）。

### 重新生成报告

```bash
cd e2e && npx tsx scripts/generate-report.ts
# watch 模式
npx tsx scripts/generate-report.ts --watch
```
