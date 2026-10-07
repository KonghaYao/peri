# 真实浏览器 E2E 记录

日期：2026-10-06。环境：本地 workerd、实际 SDK WASM、用户提供的外部 Turso 与 Anthropic 协议模型配置。操作脚本为 `scripts/e2e.mjs`；不替换 HTTP、WebSocket、Yjs、模型或数据库。

## 结论

**尚未达到稳定全绿。** 编译后的 Worker 在 8792 完整通过一轮，未出现 HTTP 5xx、pageerror 或同步错误；默认地址 8791 的后续复跑仍捕获到列表 I/O 500 或执行阶段的 Store transport 错误。因此不能把单轮 PASS 当作原问题已完全解决，也没有生产部署验收结论。

## 完整通过的一轮

1. 从空浏览器上下文打开页面，经设置界面填写令牌并保存。
2. 通过输入框创建实际 ACP 会话，等待第一轮真实回答。
3. 发送第二轮，验证模型记住上一轮唯一测试标识。
4. 验证实际 Yjs/WS snapshot 与 update；令牌不进入 WS URL。
5. 刷新页面、选择历史、切换新聊天再返回，逐字比对已完成回答。
6. 请求长回答，收到真实文本后点击停止，等待服务端关闭与停止状态。
7. 在同一聊天继续发送，验证部分回复没有被修改。
8. 再次刷新，核对四轮历史、取消状态和部分文本。
9. 在 390×844 视口操作侧栏、历史与输入框，验证无水平溢出。

## 已有确定证据的修复

- WASM 的 Node DNS lookup、socket 本机绑定和即时调度不直接照搬到 Workers；平台适配留在应用，SDK 源码不增加修改。
- 每个 Host 独立实例化 WASM，并在关闭后清理其拥有的 TCP socket，不共享带 I/O 的全局运行时。
- 真实 Turso 返回了 `compound SELECT queries not supported yet in WHERE clause subqueries`。共享 `READ_DELIVERY` 查询改用三个独立 EXISTS 的 OR，保留同一查询和身份存在性规则。
- 实测停止确认耗时 7971ms，原先 5 秒期限会提前报告失败。Stop/Resume 控制期限改为 20 秒，整体取消上限保持有界；没有取消真实 Host 关闭与 SDK settlement 的确认要求。
- 历史选择使用实际聊天 ID，避免自动标题变化造成测试选错或找不到会话。
- 默认本地启动构建并运行实际 Worker；保留独立 `dev:hmr` 入口，不把 ModuleRunner 的外部 I/O 表现视作生产 Worker 行为。

## 仍然开放的问题

`CF-LOCAL-IO-001`：复跑中仍出现 `remote session store transport`、`Network connection lost` 或带 reference 的内部错误。它们发生于实际存储 I/O；当前证据不足以把责任单独归到 Turso 服务、Rust HTTP 驱动、Workers TCP adapter 或本地模拟器。

独立的 Bun 只读控制请求中，`/v2/pipeline` 的 `SELECT 1` 返回 200，而一次 `/v3/cursor` 的只读请求在 10 秒上限内未取得响应。这是需要继续核对的协议链路差异，不是已经确定的服务端归因。

下一步应在同一请求中关联 Store HTTP 操作、底层连接结束与 Host 生命周期，区分远端 HTTP 错误和本地 I/O 异常，再建立可重复失败的最小用例。不能通过跳过失败、自动重放发送、修改期望值或仅延长超时把验收变绿。

## 其他验证

- 应用行为与展示测试：299 passed，0 failed。
- 原生 Rust `delivery_query` 回归：4 passed，0 failed，包括独立存在性来源与目标身份校验。
- TypeScript typecheck 与完整构建通过。
- 临时 Rust/依赖诊断改动已恢复，密钥文件保持忽略提交；测试不输出令牌、HTTP body 或 WS payload。

测试会留下带标识的真实会话，未删除用户已有历史。复跑命令与费用、安装条件见 `E2E.md`。

## 实例资源接口验证（2026-10-06）

新增 `GET /api/chats/:id/resources` 后，在本地编译 Worker 的 8791 端口使用真实 headless Chromium、已有本测试会话和实际 SDK WASM 验证：未鉴权 401；Host 尚未启动时实例为 null；发送命令 202；启动中的资源查询 200，导出线性内存为 27,525,120 字节（420 页，26.25 MiB）；取消 200；关闭后的查询 200，同一实例具有实际 SDK generationId，内存标记为 last-observed。CPU 明确为不可用，没有伪造 CPU 时间或占用率。查询和取消之间没有收到此接口的 5xx。

该定向验证在启动阶段取消，不依赖模型回答完成，不证明上述完整聊天 E2E 的不稳定 Store I/O 已修复。完整 E2E runner 已增加运行中/完成后资源契约检查，但本次定向通过不能替代该 runner 的全流程通过。

## WASM 审计修复复验（2026-10-07）

- CF 专用 `cloudflare` feature / `cf-wasm-size` 产物经过源码、Cargo.lock、实际工具链、参数与产物哈希的 provenance 校验；在构建中改变源码时确实拒绝发布，冻结后重新调用编译器并通过。实际 WASM 为 13,557,133 字节，初始线性内存 16 MiB、最大 64 MiB、栈 8 MiB。未编译 speed profile，不声称运行时 CPU/吞吐优化百分比。
- 应用最终 401 项测试、类型检查、应用构建通过；真实 workerd WS 测试覆盖 socket 没有 `bufferedAmount`、ACK 信用限制、超时、关闭确认前保留预算，以及强制休眠后的 attachment/订阅恢复。该组 ACP 使用 fixture，不是真实 WASM。
- 新 CF WASM 的完整本地 sqld + 模型 fixture + workerd smoke 通过：创建前空列表、真实会话 ID 与 TS 直读列表、外部改名、Yjs WS 同步、两轮对话、Worker 重启后历史与 ACP 上下文、部分输出取消、显式继续。正常完成三轮，另有一轮取消；主模型调用四次、预测调用四次。fixture 明确设置 OpenAI provider，不继承真实 `.dev.vars` 的 Anthropic 配置。
- 原生 Fetch adapter 的真实 Emscripten runtime runner 11 项契约通过，覆盖状态/请求头/请求体、流结束、原因链、取消、future/body 丢弃、超时与队列/原始 chunk 上限。使用受控 Fetch，不证明远端服务可用。
- 原生 `peri-model` 173 项测试、Store delivery query 10 项测试通过；定向 hook executor 20 项、SkillPreload 模块 25 项通过。更宽 `skill_preload` 筛选另有三个 SubAgent 集成失败，未作为本修复通过结果，也未越界修改。
- 根 workspace check、fmt、clippy、typos 和 22 条依赖方向门通过（存在既有警告）。暂存区检查未包含真实令牌、环境文件或生成产物；SDK 源码没有新增改动。

真实浏览器复验仍在创建聊天阶段遇到 `POST /api/chats` 500，诊断为 `Network connection lost.`；没有进入首轮模型回复。单独命名的探测确认失败请求仍已在 canonical Store 中创建会话，不能重放该创建命令。完整远端 E2E 当前不能标记通过；本地 fixture smoke 的成功不覆盖这个失败。此处保留失败记录，后续定位及复验另附结果，不删除先前失败。

### 编译后本地启动对照与修正

相同冻结产物、配置及本地 DNS 映射下，Vite preview 的命名创建请求返回 500，但会话已成功创建/改名，随后列表读取仍为 200；独立 Wrangler 创建返回 201，TS 查询 `/v3/cursor` 和关闭 `/v3/pipeline` 均为 200。因此默认 `dev` / `preview` 改为 Wrangler 加载编译后的 Worker，不重放失败命令，也不修改 SDK。精确的 Vite preview Fetch/响应体失败边界未取得 trace，不能据此断言某项 Turso TCP/TLS 平台能力完全不支持。

重新启动 Wrangler 后另起一次明确的新浏览器测试：页面登录和列表通过，创建 201、发送 202、Yjs 同步连接建立；首轮执行随后报 Rust `store is unavailable` / `error sending request`，失败收尾中的 exact-target Stop 超时，界面显示错误，runner 以非零退出。没有正常回答，也没有进入后续上下文、取消、继续和移动布局验收；未知停止保持禁止执行，不强行恢复或重复发送。

这次本地启动修正消除了本次观察到的创建 500，但没有解决远端 Store 在聊天执行阶段的网络失败。真实远端全流程仍未通过；当前提交仅以已通过的契约、真实 WASM fixture smoke 和上述明确失败记录作为证据，不宣称生产可用。

只读源码核对表明 Store 的 `/v3/cursor` 是 POST；当前 Turso driver 使用私有 `reqwest::Client`，上游将发送错误格式化为字符串，后续 ACP 已无法还原丢失的底层 source。现有日志不足以区分 DNS、TLS 或请求 I/O context，不能把某个猜测写成根因。模型 Native Fetch 不更换该 Store 路径；进一步修复需补齐 Store HTTP 可替换边界和远端失败的可复现实验，不在本提交加入未经验证的第三方 fork、平行 Hrana 实现或自动请求重放。
