export const meta = {
  name: 'lsp-fix-workflow',
  description: 'LSP 缺陷修复：plan → 11 个小 fix 单元 → review → rework → verify（全 sonnet）'
}

const DISCIPLINE = `
纪律（必须遵守）：
- 只修改本单元列出的文件与函数；不得顺手重构、重命名、格式化无关代码。
- 必须为本单元新增针对性测试（贴近现有测试风格，如 *_test.rs）。
- 完成前运行窄范围测试并确保通过（cargo 命令设长 timeout，如 600000ms）。
- 不要 git commit，不要改动 git 配置。
- 先读：根 AGENTS.md、docs/standards/rust.md、docs/design/testing-standards.md、peri-middlewares/CLAUDE.md（若存在）中与本次改动相关的部分。
- 完成后用 3-5 行报告：改动文件、新增测试、测试结果、任何偏离本单元的意外发现。`

phase('plan')
const plan = await agent(`你是计划者。Perihelion（Rust workspace，终端 AI 编程助手）的 LSP 工具缺陷审查已完成，缺陷清单已定。你的任务是产出执行计划与风险清单，只读调研，不写代码：
1. 快速核验关键证据：读 peri-lsp/src/diagnostics.rs:127-153、peri-lsp/src/client.rs:82-180/381-423、peri-lsp/src/jsonrpc/transport.rs:243-316、peri-lsp/src/pool.rs:60-285、peri-middlewares/src/lsp/tool.rs:97-184、peri-lsp/src/config.rs:61-124、peri-middlewares/src/plugin/loader.rs:620-648、宿主装配点（grep assembly.rs 中 LspMiddleware、lsp_servers），确认 file:line 与修复方向成立。
2. 对以下 11 个修复单元逐个给出：实现要点（2-4 条）、涉及文件、依赖的前置单元、风险（含与 5 月历史死锁/断线修复 DispatchState 的交互）、测试落点：
F1 诊断注册表改存服务器全量（diagnostics.rs）
F2 peri-lsp 新建 uri.rs：path_to_uri（绝对化+percent-encode）+ uri_to_path，pool.rs root_uri 应用
F3 middlewares 侧 tool.rs/middleware.rs/formatters.rs 应用 URI 工具、消除双重 file:// 前缀
F4 transport.rs：服务器请求回 -32601 + close() kill 子进程兜底
F5 client.rs start() 原子化（Starting 状态）+ pool ensure_server_for_file 互斥
F6 didOpen 接入工具请求路径（tool.rs + client.rs）
F7 重启计数退避 + try_restart 清 DiagnosticsRegistry
F8 startup_timeout 配置透传消费（client.rs:166 硬编码 30s）
F9 LspServerPool 会话级复用或 Drop/shutdown 清理（参照 workflow_middleware 会话级模式）
F10 plugin loader 改走 lsp_config_from_plugin（恢复 env 注入），删死代码
F11 load_global_lsp_config 接入宿主装配，对齐 MCP 三级合并
3. 指出单元间隐藏依赖或遗漏的冲突（如哪些单元会与 DispatchState 死锁修复交互、diagnostics 是否需要新建 clear 接口、F2 与 F3 跨 crate 边界）。
输出：每单元一段（编号/要点/风险/测试落点）+ 执行顺序建议 + 风险清单。中文。`, { label: 'plan', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash'] })

phase('fix')

const fixes = {}

fixes.fix01 = await agent(`${DISCIPLINE}

问题 C1（Critical）：peri-lsp/src/diagnostics.rs:127-153，handle_publish_diagnostics 把 current 存成"过滤 delivered 后的新增集合"而非服务器发布的全量，导致同文件第二次部分重叠的 publishDiagnostics 会丢失仍有效的诊断；且与上次完全相同集合时提前 return（:145-147）残留陈旧诊断。get_for_file/get_all/summary（:173-207）全部基于 current。
修复方向：current 应以服务器发布（含空集合=清除）的完整集合为准写入；delivered 仅用于"去重推送事件"（若 on_update 无调用者可简化但保留语义）；空集合 publish 应清除该文件诊断。
交付：改 peri-lsp/src/diagnostics.rs；在 diagnostics_test.rs 新增"部分重叠更新保持全量"（publish [e1,e2] → publish [e2,w3]，断言 get_all 含 e2 与 w3 两条）与"相同集合去重不残留陈旧"用例；跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-01:diagnostics', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix02a = await agent(`${DISCIPLINE}

问题 C2/H2 根因：URI 构造无绝对化、无 percent-encode。pool.rs:75 root_uri: format!("file://{}", cwd)；相对路径被拼成 file://rel（RFC 3986 下 authority 错乱）；空格/中文路径 parse 失败被 client.rs:151-153 静默降级 file:///tmp。已实证 "file:///Users/a b.rs"、中文路径 parse 失败。
本单元（只动 peri-lsp，middlewares 侧由后续单元改）：新建 peri-lsp/src/uri.rs，提供：
- path_to_uri(path: &Path) -> String：绝对化（相对路径基于当前工作目录 resolve）后 percent-encode（空格/非 ASCII/#/?/%% 等，保留 / 分隔符），输出完整 file:// URI；若输入已是 file:// 前缀则直接返回。
- uri_to_path(uri: &str) -> String：strip file:// 前缀 + percent-decode（为 F3 formatters 准备）。
参考仓库已有 lsp-types 0.97 / fluent-uri 0.3.2 的能力（或手动 percent-encode，选最简单可靠的）；在 lib.rs 导出；把 pool.rs:75 的 root_uri 改用 path_to_uri。
交付：新建 peri-lsp/src/uri.rs + 单测（空格/中文/相对路径/已带前缀输入用例）+ lib.rs 导出 + pool.rs 应用；跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-02a:uri-utils', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix02b = await agent(`${DISCIPLINE}

前置：peri-lsp/src/uri.rs 的 path_to_uri/uri_to_path 已由上一单元完成并导出（peri-lsp crate）。
本单元（只动 peri-middlewares）：全面应用 URI 工具：
- peri-middlewares/src/lsp/tool.rs:97-103 file_to_uri：统一走 path_to_uri（绝对化+编码，兼容已带 file:// 的输入）。
- tool.rs:129 与 tool.rs:184：删除错误的 format!("file://{}", self.pool.root_uri()) 双重前缀——pool.root_uri() 已是完整 file:// URI，直接使用；确认不再出现 file://file://。
- middleware.rs:73：同样统一走 path_to_uri。
- formatters.rs:13-18 uri_to_path：改用 peri-lsp 的 uri_to_path（percent-decode；注意 Windows 盘符 file:///C:/ 的处理）。
交付：改动 + 补测试（空格/中文路径、相对路径、无 file://file:// 残留），跑 cargo test -p peri-middlewares --lib lsp 与 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-02b:uri-apply', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix03 = await agent(`${DISCIPLINE}

问题 H3（协议违规）+ H1 子项（孤儿进程）：
- peri-lsp/src/jsonrpc/transport.rs:262-284 dispatch 把带 id 消息一律当响应处理，pending 找不到 sender 即静默丢弃——服务器发起的请求（workspace/configuration、client/registerCapability 等）按 LSP 规范必须回响应（未知方法回 -32601 MethodNotFound），当前静默丢弃会导致服务器同步等待、后续 textDocument 请求排队至 10s 超时。
- transport.rs:243-249 close() 只 abort read task 不 kill 子进程（child.kill() 只在自然 EOF 路径 transport.rs:177 触发），abort 路径下子进程成为孤儿。
本单元：① dispatch 增加"未知 id 的请求 → 构造 -32601 错误响应写回"分支（注意与响应区分的判据：method 字段存在且 pending 无此 id）；② close() 先尝试 child.kill()（短等待）再 abort read task，避免孤儿。
交付：改 peri-lsp/src/jsonrpc/transport.rs（及需要处）+ 新增测试：喂 {"id":1,"method":"workspace/configuration","params":[]} 应产生 -32601 响应而非静默；close 后子进程退出（可用 sleep 伪进程脚本）。跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-03:transport', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix04 = await agent(`${DISCIPLINE}

问题 H4：client.rs:82-102 start() 的 state 检查与 do_start 非原子（client.rs:88 设置 ServerState::Starting 的代码被注释，1da71cbb 遗留），并发 start 会双重 spawn 子进程；pool.rs:144-150 ensure_server_for_file 的 initialized 检查-插入同样非原子。
本单元：① start() 内先原子检查并设置 Starting 状态（或 tokio 惯用的 async 互斥/OnceCell），确保并发调用只有一个 do_start，其余等待完成或直接复用；② pool.rs ensure_server_for_file 同样加互斥。
⚠️ 死锁教训：5 月曾因持 tokio Mutex guard 跨 await 死锁（当时拆出 Arc<DispatchState>，client.rs:135-148 后台循环模式）——不得在持有锁时跨 await 调用 client 方法；若锁必须跨 await，用 async Mutex 或先复制需要的数据再 await。
交付：client.rs + pool.rs 改动 + 并发启动测试（tokio::join! 两个 start，断言只 spawn 一次子进程）。跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-04:start-atomic', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix05 = await agent(`${DISCIPLINE}

问题 M1：client.rs:266 的 did_open 定义后全仓无调用点；工具请求前从不 didOpen，未编辑过的文件服务器无文档状态——documentSymbol/hover/goToDefinition 空结果、publishDiagnostics 不推送。
本单元：在 peri-middlewares/src/lsp/tool.rs:243-455 各查询操作前确保文件已 didOpen：
- 实现"已打开"缓存（client 侧 opened_files HashSet + 幂等 did_open，或 tool 侧记录），只对实际存在的文件发送 didOpen（需读文件内容传入，参考现有 did_change 用法 middleware.rs:82）；文件内容读取失败则跳过 didOpen 不阻塞查询。
- languageId 用现有 infer_language_id。
- 重启后（try_restart）打开缓存应重置（若 F7 已改则保持一致，否则本单元顺带清缓存并注明）。
交付：client.rs（如需要）+ tool.rs 改动 + 测试（首次查询触发 didOpen 且不重复；didOpen 内容正确）。跑 cargo test -p peri-middlewares --lib lsp 与 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-05:didopen', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix06 = await agent(`${DISCIPLINE}

问题 M3 + M4（重启语义）：
- M3：client.rs:394-405 check_and_increment_restart + :419-423 try_restart 成功即清零 restart_count，持续崩溃的服务器无退避，max_restarts（默认 3，pool.rs:55）形同虚设。
- M4：try_restart（client.rs:417）只清 open_files 不清 DiagnosticsRegistry，重启后旧诊断残留，且不重新 didOpen（若 F5 已加打开缓存，重启后应清缓存使下次请求重新 didOpen）。
本单元：改 client.rs：① 重启计数采用时间窗退避（如 60s 窗口内不重置计数、窗口过后才清零，或简单指数退避），超出 max_restarts 返回 ServerCrashed 并冷却；② try_restart 内清 DiagnosticsRegistry（若 peri-lsp/src/diagnostics.rs 缺 clear 方法则新增，含测试）并重置打开文件/已打开缓存。
交付：改动 + 测试（重启计数窗口语义、重启后诊断清空、冷却期不重启）。跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-06:restart', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix07 = await agent(`${DISCIPLINE}

问题 M7：peri-acp-types/src/lsp.rs:57 的 startup_timeout 配置字段从未被消费，client.rs:166 initialize 超时硬编码 30_000ms——用户配置的 startupTimeout 无效。
本单元：把 startup_timeout 从配置透传到 LspClient.start（缺省保持 30s）：client.rs:166 改读参数；从 LspServerConfig（peri-acp-types/src/lsp.rs）→ peri-lsp client 的透传链路打通。若跨 crate 链路过长，可先打通 peri-lsp 内部（config 解析处传参）并注明剩余环节。
交付：改动 + 测试（超时参数生效：短超时+慢服务器触发超时，或断言参数传递正确）。跑 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-07:timeout', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix08 = await agent(`${DISCIPLINE}

问题 H1：chain/pool 每 turn 重建（stage_builder.rs:511 → assembly.rs:411 → middleware.rs:24 每 turn 新建 LspServerPool）；pool.rs:219 shutdown 全仓无调用点；LspClient/LspServerPool/MessageDispatcher 均无 Drop impl——服务器进程泄漏、每 turn 冷启动数秒、跨 turn 状态（initialized/诊断/open_files）全丢。（kill 兜底已由 transport.close 修复处理）
本单元：提升 pool 生命周期，先调研现状再选方案、小步实现：
方案 A（推荐，若改动可控）：参照 peri-agent 中 workflow_middleware 的"会话级端口，None=临时实例"既有模式（grep factory.rs 附近），让 LspServerPool 在会话/宿主级别共享，并注册 shutdown 钩子（宿主退出时调 pool.shutdown()）。
方案 B（保守降级）：至少保证 (a) LspServerPool/LspClient 实现 Drop 时杀子进程（或 middleware 析构时调 shutdown）；(b) 宿主装配处显式调用 pool.shutdown()（找到 assembly/launch 生命周期锚点，参照 peri-tui/src/launch.rs:237 关闭 MCP pool 的做法）。
明确说明选了哪个方案及理由。交付：改动 + 生命周期测试（drop/关闭后子进程退出）。跑受影响 crates 测试全绿。`, { label: 'fix-08:pool-lifetime', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix09 = await agent(`${DISCIPLINE}

问题 M5：plugin/loader.rs:630-643 手写构造 LspServerConfig 且 env: None（丢失 env 注入）；peri-lsp/src/config.rs:94-124 的 lsp_config_from_plugin（会注入 CLAUDE_PLUGIN_ROOT）全仓无生产调用者（死代码）——依赖插件根相对命令/环境变量的插件 LSP server 启动失败。
本单元：loader.rs 改走 lsp_config_from_plugin 的转换逻辑（复用其 env 注入），删除重复转换实现；保留插件 manifest 字段映射（command/args/extensions 等）；若 lsp_config_from_plugin 有缺陷则修复而非新写。
交付：改动 + 测试（env 注入断言：配置含 CLAUDE_PLUGIN_ROOT 或插件根环境变量）。跑 cargo test -p peri-middlewares --lib lsp 与 cargo test -p peri-lsp --lib 全绿。`, { label: 'fix-09:loader-env', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix10 = await agent(`${DISCIPLINE}

问题 H5（契约偏差）：peri-lsp/src/config.rs:61 load_global_lsp_config（settings.json 的 config.lspServers）无生产调用者；宿主装配只取插件 lsp_servers（peri-acp/src/host/assemble.rs:226-228、stdio/init.rs:82），无插件时 LSP 整条产品线静默不可用。对照 MCP 有三级合并（全局 settings.json + 插件 + 项目，见 peri-middlewares/src/mcp/config.rs:228-232 与 CLAUDE.md:14）。
本单元：把 load_global_lsp_config 接入宿主装配：全局配置与插件配置合并（合并优先级对齐 MCP 语义，先读 MCP 合并实现），装配处 lsp_servers 非空条件不变；settings.json 读取路径与宿主现有全局配置加载机制对齐（搜索宿主如何读全局 settings）。注意 load_global_lsp_config 若读取路径与宿主不一致需适配。
交付：改动 + 装配级测试（无插件但全局配置存在时 LspMiddleware 注册/工具可用）。跑 cargo test -p peri-acp --lib（或受影响 crate）与相关测试全绿。`, { label: 'fix-10:global-config', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

fixes.fix11 = await agent(`${DISCIPLINE}

问题 L 系列残留清理（低危，一并处理，保持小步）：
- L2：client.rs:202-250 请求超时后 oneshot sender 残留 pending map（仅 EOF 时 reject_all_pending 清理）→ 超时路径同时从 pending 移除。
- L7：peri-lsp/src/diagnostics.rs:89-91 on_update 注册后无调用者（死代码）——若 F1 已保留该语义则接线（发布/清除事件触发 on_update，含空集合清除通知）；若确认整个回调链无消费者，删除死代码并注明。
- 顺带确认 codec.rs:38 Content-Length 头大小写兼容（小写 content-length 不丢帧）与 :58 上限防护（如 64MB 上限，超限报错而非大分配）。若改动超出合理范围，报告即可不强改。
交付：上述三项各自最小改动 + 对应测试；跑 cargo test -p peri-lsp --lib 与 cargo test -p peri-middlewares --lib lsp 全绿。`, { label: 'fix-11:low-hanging', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

phase('review')
const review = await agent(`你是 code reviewer。上一阶段完成了 Perihelion LSP 缺陷修复（11 个小单元）。请审查全部改动（git diff HEAD 与 git diff --stat 概览，再阅读改动文件），核对：
1. 修复是否正确实现目标：诊断全量语义、URI 绝对化+编码（无双重 file://）、服务器请求回 -32601、start 原子化（无死锁回归）、didOpen 接入、重启退避+清诊断、startup_timeout 透传、pool 进程清理、loader env 恢复、全局配置接线。
2. 是否引入新回归：死锁/竞态（尤其持锁跨 await）、孤儿进程、行为不一致、与 5 月 DispatchState 修复冲突、公共 API 意外变化。
3. 测试是否真的覆盖声称的行为（读测试代码验证，不要只看测试存在）。
只报告"必须修复"的问题（阻止性错误、明确逻辑 bug、明显回归），风格建议不报。每条给 file:line + 理由 + 最小修复建议。若没有必须修复项，明确说"无阻止性问题"。中文。`, { label: 'review', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash'] })

phase('rework')
const rework = await agent(`${DISCIPLINE}

下面是 code review 意见（可能为空或全为建议）。逐条处理：
${review}

要求：每条保持最小改动；每条修复后跑对应窄范围测试；不要 commit；最后报告每条的处理结果（已修复 / 解释为何不改）。若意见为空或全是建议，直接报告"无需修复"，并跑一次 cargo test -p peri-lsp --lib 确认现状。`, { label: 'rework', model: 'sonnet', allowedTools: ['Read', 'Edit', 'Write', 'Grep', 'Glob', 'Bash'] })

phase('verify')
const verify = await agent(`你是验证者。Perihelion LSP 修复已完成（plan/fix/rework 全部结束）。做最终验证，只验证不改代码（测试笔误除外）：
1. cargo test -p peri-lsp --lib（长 timeout，600000ms）
2. cargo test -p peri-middlewares --lib lsp
3. cargo test -p peri-acp --lib（若装配/宿主被改）
4. cargo clippy -p peri-lsp -p peri-middlewares -p peri-acp --all-targets -- -D warnings（某 crate 编译太久则报告并继续，不强行跑完）
5. git status 与 git diff --stat 汇总改动面
报告：每项结果（通过/失败+关键错误摘要，失败时给出 3-5 行错误定位）、改动文件清单、残留风险与建议。中文。`, { label: 'verify', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash'] })

const summary = [
  'PLAN: ' + (typeof plan === 'string' ? plan.slice(0, 400) : ''),
  'FIX01: ' + (typeof fixes.fix01 === 'string' ? fixes.fix01.slice(0, 200) : ''),
  'FIX02a: ' + (typeof fixes.fix02a === 'string' ? fixes.fix02a.slice(0, 200) : ''),
  'FIX02b: ' + (typeof fixes.fix02b === 'string' ? fixes.fix02b.slice(0, 200) : ''),
  'FIX03: ' + (typeof fixes.fix03 === 'string' ? fixes.fix03.slice(0, 200) : ''),
  'FIX04: ' + (typeof fixes.fix04 === 'string' ? fixes.fix04.slice(0, 200) : ''),
  'FIX05: ' + (typeof fixes.fix05 === 'string' ? fixes.fix05.slice(0, 200) : ''),
  'FIX06: ' + (typeof fixes.fix06 === 'string' ? fixes.fix06.slice(0, 200) : ''),
  'FIX07: ' + (typeof fixes.fix07 === 'string' ? fixes.fix07.slice(0, 200) : ''),
  'FIX08: ' + (typeof fixes.fix08 === 'string' ? fixes.fix08.slice(0, 200) : ''),
  'FIX09: ' + (typeof fixes.fix09 === 'string' ? fixes.fix09.slice(0, 200) : ''),
  'FIX10: ' + (typeof fixes.fix10 === 'string' ? fixes.fix10.slice(0, 200) : ''),
  'FIX11: ' + (typeof fixes.fix11 === 'string' ? fixes.fix11.slice(0, 200) : ''),
  'REVIEW: ' + (typeof review === 'string' ? review.slice(0, 300) : ''),
  'REWORK: ' + (typeof rework === 'string' ? rework.slice(0, 300) : ''),
  'VERIFY: ' + (typeof verify === 'string' ? verify.slice(0, 400) : ''),
].join('\n')

return summary
