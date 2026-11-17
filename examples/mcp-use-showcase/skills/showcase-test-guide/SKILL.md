---
name: showcase-test-guide
description: 按步骤验收 mcp-use showcase MCP：tools、resources、通知/订阅、错误路径、MRTR、skills 与 prompts；每步给出调用参数、预期结果和失败判定。
license: MIT
compatibility: 需要支持 resources 的 MCP host；Peri 支持 subscriptions/listen 与资源通知；MRTR、skills、自定义通知展示因宿主而异
metadata:
  owner: peri-examples
  level: demo
---

# mcp-use showcase 验收指南

目标：用最少调用确认这个 MCP server 的每类能力可用，并留下可核对的 PASS / FAIL / SKIP 记录。

规则：

1. 只依据工具和资源的实际返回判定，不根据描述猜测。
2. 宿主不支持的能力记 SKIP，不要记 PASS，也不要记 FAIL。订阅不是可选能力：Peri 默认支持，只有实际调用返回不支持时才 SKIP（见第 3 步）。
3. 不要为了“看效果”反复调用破坏性工具；清理步骤只在最后执行一次。
4. 工具名可能被宿主加命名空间前缀；以 `tools/list` 中名称的后缀匹配本指南里的工具名。
5. 先区分“server 未发送”和“宿主未展示”：会话里看不到通知时，用独立 MCP 客户端（`scripts/client.ts`、`scripts/subscribe.ts` 或 SDK）复测；只有 server 也收不到才记 FAIL，宿主未展示记 SKIP。

## 0. 前置检查

1. 确认 server 已启动：`http://127.0.0.1:31337/health` 返回 `{"ok":true,...}`。
2. 调用 `tools/list`，应能看到 `echo`、`search_notes`、`create_note`、`set_theme`、`slow_task`、`request_approval`、`summarize_note_with_host_model`、`client_info`、`fail_demo`、`get_testing_guide`。
3. 确认 `_internal_diagnostics` **不** 出现在列表中；这是中间件过滤的预期结果。
4. 读取 `demo://state`，记录初始 `revision` 与 `theme`，后续用它判断状态是否推进。

## 1. 工具基础面

1. 调用 `echo`：

   ```json
   { "text": "hello", "uppercase": true }
   ```

   预期：文本内容为 `HELLO (5 chars)`，`structuredContent` 为 `{"text":"HELLO","length":5}`。宿主可能只展示 text content；`structuredContent` 用独立 SDK 客户端（`client.callTool` 的返回）核对，不要在宿主不展示时判 FAIL。

2. 调用 `search_notes`：

   ```json
   { "tag": "mcp" }
   ```

   预期：`total=1`，`results[0].id="mcp-basics"`；结果只含 id/title/tags/updatedAt。

3. 调用 `client_info`，空参数。预期：返回 client 名称/版本、sampling/elicitation/views 能力布尔值、`state.revision`。

失败判定：工具未列出、参数校验拒绝合法参数、或独立客户端返回缺少 `structuredContent`。宿主只展示 text 时用独立客户端核对。

## 2. 资源读取

依次读取并检查：

| URI | 预期 |
| --- | --- |
| `demo://readme` | Markdown，含 `# mcp-use showcase` 与工具/资源清单 |
| `demo://notes` | JSON 数组，至少含 `mcp-basics`、`stateless`、`peri` |
| `demo://notes/peri` | JSON 对象，`id="peri"`，含 title/body/tags |
| `demo://report` | 两个内容块：Markdown 报告 + JSON 状态 |
| `demo://banner` | `image/png`，`blob` 非空 |
| `demo://notes/does-not-exist` | 资源错误，不是空内容 |

失败判定：URI 不存在、模板参数没有替换、或读取后内容与 state 不一致。

## 3. 状态推进与订阅通知

Peri 默认支持 MCP 2026-07-28 的 `subscriptions/listen`。demo 目录的 `.mcp.json` 已声明：

```json
"subscriptions": {
  "resources": ["demo://state"],
  "resourcesListChanged": true,
  "toolsListChanged": true
}
```

`subscriptions` 不是 initialize 的 client capability，`client_info.capabilities` 里没有对应字段；不要因为字段缺失而 SKIP。判定依据是 listen 被接受（收到 `notifications/subscriptions/acknowledged`，`honoredFilter` 非空）以及通知实际到达。

1. 读取 `demo://state`，记录 `revision=R`。
2. 确认订阅已建立：Peri 启动时按 `.mcp.json` 发起 `subscriptions/listen`，参数为 `resourceSubscriptions: ["demo://state"]`、`resourcesListChanged: true`、`toolsListChanged: true`，并收到 acknowledged filter。没有可见事件时，以第 4、6 步的通知到达作为证据。
3. 调用 `set_theme`：

   ```json
   { "theme": "dark" }
   ```

   预期：`structuredContent.revision=R+1`，`theme="dark"`。
4. 预期收到 `notifications/resources/updated`，`params.uri="demo://state"`。
5. 再次读取 `demo://state`，预期 `theme="dark"`、`revision=R+1`。
6. 调用 `create_note`：

   ```json
   { "id": "release-checklist", "title": "发布检查", "body": "验证通知链路", "tags": ["demo"] }
   ```

   预期：`created=true`，`revision` 递增；读取 `demo://notes/release-checklist` 能拿到新内容。`notifications/resources/list_changed` 不带 `uri`，不要用 URI 判定；Peri 收到后主要用于失效资源缓存，**不保证**在会话中注入可见提醒。需要核对 server 是否发出时，用独立客户端（`scripts/subscribe.ts` 或 SDK）执行同一 listen 过滤器，确认 `honoredFilter` 非空且收到 `notifications/resources/list_changed`；会话无提醒不判 FAIL。

只有以下情况记 SKIP，并在报告中附实际返回：

- `subscriptions/listen` 返回方法不支持，例如 `Method not found`。legacy `resources/subscribe` 在 mcp-use stateless 的 2026 端点上同样返回 `Method not found`，不能据此判 server 失败。
- listen 响应没有 acknowledged filter（`honoredFilter` 为空），说明订阅未被确认。
- 宿主不是 Peri，且有实际调用证据表明它不发起订阅。

失败判定：独立客户端已确认 listen 且 `set_theme` 后没有收到 `notifications/resources/updated`，或通知方法名与 URI 不符；状态没有递增、重新读取仍是旧值。宿主未把 `list_changed` 注入会话只记 SKIP，不判 FAIL。

## 4. 长任务、进度与自定义通知

调用 `slow_task`：

```json
{ "steps": 3, "stepMs": 50 }
```

预期：

1. 最终文本包含 `完成` 和 `3 步`。
2. 若请求携带 `progressToken`，server 会发送 `notifications/progress`。`slow_task` 返回文本中的 `进度通知支持=true` 即证明宿主提供了 progressToken 且进度已发送；Peri 当前不把 progress 注入模型会话，不要求会话可见。
3. server 会发送 3 条以上 `demo/task-status` 通知，状态依次为 `started`、`running`（多次）、`completed`。Peri 当前不会把这些自定义通知注入模型会话；用独立客户端验证 server 发送，宿主侧记 SKIP（未展示）。
4. server 会发送 `notifications/message`，`logger="showcase"`。该通知在 MCP 2026-07-28 已废弃（SEP-2577），仅当请求 `_meta` 带 `io.modelcontextprotocol/logLevel` 才应发送；demo 仍会发送用于演示。宿主不展示时记 SKIP，不判 server FAIL。

判定口径：最终结果与 progress 支持标志可在宿主会话内验证；`demo/task-status` / `notifications/message` 属于宿主展示能力，Peri 当前明确忽略（`peri-middlewares/src/mcp/client/subscription.rs`），必须用独立客户端确认 server 侧发送。不要把“宿主未展示”记成 server FAIL。

失败判定：任务最终结果错误；`进度通知支持=true` 但独立客户端收不到 `notifications/progress`；或独立客户端也收不到 `demo/task-status` / `notifications/message`。宿主未展示只记 SKIP。

## 5. 错误路径

1. 调用 `echo` 传空对象 `{}`：应返回参数校验错误，server 保持可用。
2. 调用 `fail_demo`（可传 `{"message":"expected"}`）：预期 `isError=true`，文本以 `预期失败：` 开头。
3. 调用 `search_notes` 传一个不存在的 `tag`：预期成功返回 `total=0`，不是错误。
4. 完成上述错误后再次调用 `echo`，确认 server 没有崩溃。

失败判定：错误被吞掉、错误后连接不可用、或成功路径被误判为错误。

## 6. MRTR：elicitation 与 sampling

这两项依赖 MRTR 宿主能力，与订阅无关；先调用 `client_info` 看 `capabilities.sampling` / `capabilities.elicitation`，再按实际返回判定。Peri rmcp 3.9.0 当前两项均为 `false`，因此本步预期为 SKIP；不要因为订阅可用就推断 MRTR 可用。

1. `request_approval`：

   ```json
   { "action": "reset-demo-state" }
   ```

   - 支持 elicitation：返回 `approved`（布尔）和可选 `note`；拒绝时 `approved=false`。
   - 不支持：返回 `isError=true`，文本说明客户端未声明 elicitation 能力。

2. `summarize_note_with_host_model`：

   ```json
   { "noteId": "peri" }
   ```

   - 支持 sampling：返回非空摘要文本。
   - 不支持：返回 `isError=true`，文本说明未声明 sampling 能力。

失败判定：宿主已声明能力但请求没有被转发；不支持时错误信息缺失能力名。SKIP 只能依据实际错误（`isError=true` 且文本说明缺少 sampling/elicitation 能力）或其他宿主能力证据，不要用订阅是否可用推断 MRTR。

## 7. Skills 与 Prompts

1. 若宿主支持 Skills over MCP：`skills/list` 应返回且仅返回 `showcase-test-guide`。宿主的列表可能只给名称、不展开 `resources`；此时直接读取下面两个资源判定，不要因缺少 `resources` 字段判 FAIL。
2. 读取 `skill://showcase-test-guide/SKILL.md`，内容应与本指南一致。
3. 读取 `skill://showcase-test-guide/references/failure-triage.md`，应包含失败分类与排查顺序。
4. 若宿主无法列出技能：直接读取上面的技能资源，或调用 `get_testing_guide`；不要因此判 server FAIL。
5. `prompts/list` 应包含 `review-note` 与 `project-brief`。Peri 当前没有 prompts 消费入口；可用独立 SDK 客户端执行 `listPrompts` / `getPrompt` 验证 server，宿主侧记录“未消费”，不判 FAIL。
6. `prompts/get` 调用 `review-note`，参数 `{"noteId":"peri","tone":"concise"}`，应返回一条 user 消息，正文含笔记标题和正文。

失败判定：技能资源不可读、frontmatter 与目录名不一致、prompts 参数校验错误。宿主不消费 prompts 时，列出即可。

## 8. 清理

仅当本次测试创建了 `release-checklist` 或修改过主题时执行：

```json
{ "confirm": true }
```

调用 `reset_demo_state`。预期：`noteCount=3`、主题恢复为 `light`；`revision` **不会回到 1**，而是继续递增，这是预期行为。清理后重新读取 `demo://notes` 确认只剩种子数据。

## 报告模板

按步骤输出，不要只给一句“通过”：

```text
| 步骤 | 结果 | 证据 |
| --- | --- | --- |
| 1 工具基础面 | PASS | echo=HELLO (5 chars); search_notes.total=1 |
| 2 资源 | PASS | demo://notes/peri.id=peri |
| 3 订阅 | PASS | set_theme → subscription_resource_updated(uri=demo://state)；独立 listen → list_changed（会话内无提醒，正常） |
| 4 长任务 | PASS（展示项 SKIP） | slow_task 完成；progress=true；独立客户端收到 demo/task-status + notifications/message；宿主未展示自定义通知 |
| 5 错误路径 | PASS | fail_demo.isError=true |
| 6 MRTR | SKIP | client_info.capabilities.sampling=false；request_approval 返回 isError，文本含 elicitation |
| 7 Skills/Prompts | PASS | skills/list=1；SKILL.md 与 failure-triage.md 可读；prompts=2（SDK 验证） |
| 8 清理 | PASS | noteCount=3 |
```

非 Peri 宿主的订阅 SKIP 必须写成可核对的形式，例如 `SKIP | subscriptions/listen 返回 Method not found`；不要写“宿主未声明订阅能力”。

FAIL 必须附工具名、参数和实际返回片段。遇到失败先读 `references/failure-triage.md`，不要靠重试掩盖问题。

## Peri 实测基线（2026-11-12，rmcp 3.9.0）

- `subscriptions/listen`：acknowledged；`resources/updated` 以会话提醒到达。
- `resources/list_changed`：server 发送；Peri 仅失效资源缓存，不注入提醒。
- `demo/task-status` / `notifications/message`：server 发送；Peri 不注入会话，记 SKIP。
- `notifications/message` 在 MCP 2026-07-28 已废弃（SEP-2577），仅请求 `_meta` 带 logLevel 时才应发送。
- MRTR：`sampling=false`、`elicitation=false`，两项 SKIP。
- Prompts：server 暴露 2 个，宿主无消费入口；用 SDK 验证。
