/**
 * `get_testing_guide` 的正文。
 *
 * 文本与 `skills/showcase-test-guide/SKILL.md` 对齐；修改验收步骤时两处同步。
 */
export const TESTING_GUIDE = [
  "# mcp-use showcase 验收步骤",
  "",
  "完整版：读取 skill://showcase-test-guide/SKILL.md；",
  "失败排查：skill://showcase-test-guide/references/failure-triage.md。",
  "",
  "1. echo {text:\"hello\", uppercase:true} → HELLO (5 chars)；structuredContent={text:HELLO,length:5}（宿主不展示时用 SDK 核对）",
  "2. search_notes {tag:\"mcp\"} → total=1，results[0].id=mcp-basics",
  "3. read demo://notes → 至少 3 条；read demo://notes/peri → id=peri",
  "4. read demo://state 记 revision=R；set_theme 切主题 → revision=R+1；再读确认",
  "5. Peri 默认订阅：按 .mcp.json 的 subscriptions/listen（resources: [\"demo://state\"]）应收到 acknowledged filter；set_theme → notifications/resources/updated(uri=demo://state)；create_note → list_changed 由 server 发送，Peri 仅失效缓存、会话无提醒，属正常，不判 FAIL。不要因 client_info 无订阅字段而 SKIP",
  "6. slow_task {steps:3, stepMs:50} → 文本含“完成”；`进度通知支持=true` 证明 progressToken 已携带。Peri 不把 demo/task-status / notifications/message 注入会话；用独立客户端验证 server 发送，宿主未展示记 SKIP，不判 FAIL",
  "7. fail_demo → isError=true；echo {} → 参数校验错误；错误后 echo 仍可用",
  "8. MRTR：request_approval、summarize_note_with_host_model；按实际能力/错误判定 SKIP，与订阅无关",
  "9. skills/list 或读取 SKILL.md；prompts/list 应含 review-note、project-brief（宿主无 prompts 入口时用 SDK 客户端验证）",
  "10. 需要清理时 reset_demo_state {confirm:true}；revision 会继续递增，不回到 1",
  "",
  "报告用 PASS / FAIL / SKIP；SKIP 不等于 PASS，FAIL 必须附实际返回。宿主未展示 ≠ server 未发送，先用独立客户端区分。",
].join("\n");
