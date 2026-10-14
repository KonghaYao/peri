export const meta = {
  name: 's7c-frontend-defect-fix',
  description: 'Fix the S7 verification defects: TUI D1 (scroll trigger wiring dead in idle sessions), D2/D3/D5/D6/D8, and SDK compact-failure convergence plus the P3 public-surface gaps',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'
const CD = 'cd ' + WORKTREE + ' && '
const CARGO = './scripts/cargo-rmcp-patched.sh'

const BRIEF = [
  'worktree（唯一工作目录，绝对路径）：' + WORKTREE,
  '所有 bash 命令必须以 `cd ' + WORKTREE + ' && ` 开头（workflow 里 bash 的默认 cwd 是主仓库 /Users/konghayao/code/ai/peri-v4p3，不是 worktree）。',
  'Cargo 一律用 `./scripts/cargo-rmcp-patched.sh`（禁止裸 cargo、禁止 --workspace）；SDK 用 bun。',
  '禁止：git 写操作（工作区保持未提交）、DDL、新增依赖（含 devDependency）、改服务端（peri-acp / peri-acp-types / peri-resources 的语义）——服务端 wire 形状已冻结。',
  '只改与本任务直接相关的文件；不要顺手重构无关代码。',
].join('\n')

const SHARED = [
  '## 输入（先读，全部是上一轮独立复核的实测结论，不要重新发明）',
  '- TUI 复核：' + WORKTREE + '/.recon/S7-tui-verification.md（第 0 节判定摘要、第 7 节 D1~D8 缺陷、第 6 节既有断言变更）',
  '- SDK 复核：' + WORKTREE + '/.recon/S7-sdk-verification.md（第 1 节结论速览、第 4 节逐条判定、第 6 节未完成项、第 7 节风险）',
  '- 实现者报告：' + WORKTREE + '/.recon/S7a-tui.md、' + WORKTREE + '/.recon/S7b-sdk.md',
  '- 服务端契约（只读参考，不要改）：' + WORKTREE + '/peri-acp/src/host/history_page.rs 模块文档',
  '- 需求文档：/Users/konghayao/Desktop/peri-transcript-history-requirements.md（§5.2 子项身份、§6 REQ-C01/C02/C03、§9 验收 A11/A12/A20/A22/A23/A24）',
].join('\n')

const RULES_COMMON = [
  '## 硬约束',
  '- 每条修复都要有**能失败的测试**支撑：先证明测试在修复前失败（或在报告里给出最小复现命令与旧行为），再给修复后的通过结果。',
  '- 不得为了让测试通过而放宽/删除既有断言；改动既有断言必须逐条在报告里说明理由。',
  '- 报告写实测命令原文与结果原文；没有命令支撑的结论不许写。',
  '- 修复后必须自跑：' + CD + CARGO + ' test --locked -p peri-tui --lib（TUI）/ ' + CD + 'bun run --cwd npm-packages/@peri-sdk typecheck && bun test（SDK，按仓库既有脚本名调整并如实记录）。',
].join('\n')

const PROMPT_TUI = [
  '你是实现者，负责 TUI 侧（peri-tui）的缺陷修复。',
  '',
  BRIEF,
  '',
  SHARED,
  '',
  '## 任务（按优先级，逐条给修复 + 测试 + 证据）',
  '',
  '### T1（P1，必做）D1 滚动触发接线不生效 —— REQ-C01-1 FAIL',
  '事实（复核已定位，请自己复核一遍再动手）：`request_older_page_if_due` 的唯一生产调用点是 `peri-tui/src/kit/scroll/auto_follow.rs:269-276`，它只在 `peri-tui/src/kit/message_area/mod.rs:388` 的 `hooks.use_effect(...)` 里跑；该 effect 的 deps（`mod.rs:415-423`）= (items_len, vm_generation, is_loading, total_visual_rows, vis_height, loading_epoch, bridge_reset_counter)，**不含滚动偏移 / follow_bottom / HISTORY_PREPEND_EPOCH**，而 ratatui-kit 的 `use_effect` 在 deps 相等时不执行 → 空闲会话里滚到顶端不会触发取更早页，翻页失败后的重试也没有触发源（运行中的 turn 掩盖了它）。',
  '要求：让「视口贴边」在滚动事件里就能触发（或把触发所需的滚动事实纳入 effect 输入），且**不得**在每帧/每次滚动无节流地发请求——并发合并与「读—判—写同锁认领」的既有机制（`history_paging.rs:461-508`）要继续作为唯一去重口。',
  '必须补一条能在修复前失败的测试：空闲会话（无 turn、无内容变化）下模拟滚动到已载入窗口顶端 → 断言确实产生了一次对应的取页请求（或等价的触发事实）。测试要落在既有测试文件风格里（`history_paging_test.rs` / `message_area` 相关测试），不要只测纯函数。',
  '',
  '### T2（P2，必做）D2 同会话 rewind 后客户端不失效浏览状态',
  '`peri-tui/src/kit/acp_events/system.rs:493-520` 的 `handle_rewind_completed` 只替换 `committed`，不重置浏览状态：rewind 时刻已在途、之后才返回的页会按旧索引并入（A11/A19 字面风险）。',
  '要求：rewind 完成后让浏览状态（区间索引 / 已载入窗口身份 / 在途认领 / 缓存的 cursor 与快照身份）失效，使迟到页被丢弃；补测试（并发触发一次取页 RPC 后立刻 rewind，断言页不被并入）。',
  '',
  '### T3（P3→功能，建议做）D5 页 RPC 无超时/取消 → 在途认领可能永久占用',
  '服务端静默时 `HISTORY_PAGING.in_flight` 永久占用，该会话翻页彻底停滞。要求：给页 RPC 加超时或取消路径，超时后释放认领并允许重试（失败只记 `last_error`、保留 `next_cursor`），补测试。',
  '',
  '### T4（P3，做）D3 首屏自有 reminder 条目不登记区间',
  '极端形状（首屏自有条目只有 reminder）会让 `insert_slot_for` 兜底返回槽位 0，更早历史被插到继承前缀之前（违反 REQ-H01-6 / A20）。要求：让这类条目也登记区间（或给出等价正确的插入槽位），补测试。',
  '',
  '### T5（P3，做）D8 「无 reload」断言可能真空',
  '`test_compact_turndone_keeps_window_without_reload`（`acp_events_test/session_events_test.rs:766-940`）的「无请求」断言在 `OnceLock` 被占用时可能真空。要求：改成不依赖全局单例占用状态的形式（例如注入 test 专用 sender），保证断言真的能失败。',
  '',
  '### T6（P3，尽力，允许如实不达成）D6 锚点用行数增量近似（REQ-C01-3 字面未满足）',
  '需求 A20 要求「阅读锚点不跳动」。当前实现用「总行数增量」补偿，机制与需求字面不同。要求：尝试实现真正的锚点（把视口顶部可见单元的身份 + 偏移，在 prepend 后恢复），补测试（prepend 前后同一可见单元的屏幕行位置不变）。',
  '若在当前架构下代价过大或不可行：**不要**假装达成——把近似机制与其局限写进代码注释，并在报告里如实标注「仍为 PARTIAL + 原因 + 修复代价估计」。',
  '',
  '### T7（P3，做）D7 注释与 code-index 未同步',
  '同步 `docs/code-index/peri-tui.md`（按仓库 `DOC-UPDATE-001` 与 code-index 规范；先读 `docs/standards/` 里对应标准与既有 code-index 文件的写法）。另外读 `docs/verification/tui-integration-cases/README.md`：若该权威目录要求为新用户可见行为登记集成用例，则按它的格式补一条「滚动加载更早历史」的用例（不要发明新格式）。',
  '',
  '### T8（P3，做）D4 未配对工具终点可能错配到已 flush 的实时卡片',
  '历史页里「结果先到、起点未到」的暂存结果可能被补到同 `tool_id` 的实时卡片上（A10 要求「不挂到实时 turn」）。要求：让暂存结果只可能配对到历史页来源的卡片（例如按来源/区间身份限定），补测试或说明为何不可达。',
  '',
  RULES_COMMON,
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7c-tui-fix.md：逐条 T1~T8 的「修了什么 / 证据（命令 + 结果）/ 未达成与原因 / 风险」；另列本轮改动的文件清单（绝对路径）与既有断言的任何变更。',
].join('\n')

const PROMPT_SDK = [
  '你是实现者，负责 SDK 侧（npm-packages/@peri-sdk）的缺陷修复。',
  '',
  BRIEF,
  '',
  SHARED,
  '',
  '## 任务（按优先级，逐条给修复 + 测试 + 证据）',
  '',
  '### S1（P2，必做）Compact 失败无收敛 —— A12「完成／失败状态正确」FAIL',
  '复核结论：手动 Compact 失败（`command_feedback` 的 error/cancel）后 SDK 既无失败展示，`compactionStatus` 又停在 `"running"`；切片自带用例用 `agent_execution_failed`（turn 失败）冒充 Compact 失败，掩盖了缺口（`tests/session-docs.test.ts:514-529`）。',
  '要求：在真实的 Compact 失败路径上收敛状态（失败展示 + `compactionStatus` 离开 `running`），并补一条走**真实 Compact 失败**（`command_feedback` error/cancel，不是 turn 失败）的用例；错误形状以服务端实际发射为准——先读服务端（`peri-acp/src/` 里 compact 的 command_feedback / 事件形状，只读）确认键名与语义，不要猜。',
  '',
  '### S2（P2，判断后处理）初始回放不再进入 `session.stream()`',
  '复核发现：staged 分支在 `src/agent/session.ts:152` 的 `this.notifications.push(event)` **之前** return，因此带 `periLoadGeneration` 的回放通知（含回放期 `peri/unstable_event`）不再出现在 `session.stream()`；改动前会出现。README:40 与 `examples/demo` 的原始事件流会缺首屏。',
  '要求：先判断这是否会破坏 A22 的暂存语义（暂存的是投影安装，不应等于丢事件）。若能安全保留「原始事件仍进 `stream()`」同时保持按代次暂存安装，就这么做并补测试；若两者不可兼得，则保留现行为并在 README / code-index / 示例里**显式记录该行为变化**（含理由与迁移提示），报告里说明取舍。',
  '',
  '### S3（P3，做）风险 3 跨路径块身份不一致',
  '页路径（`h:{position}`）与回放路径（`h:{messageId}:{type}:{n}`）对同一条目给出不同块身份 → 同一条目被两条路径投递会重复渲染（实现者已用 `test.failing` 复现）。要求：统一身份（优先用稳定身份 + 位置），把 `test.failing` 转成正常断言；若统一会破坏既有渲染，说明原因并保留可追溯的标记。',
  '',
  '### S4（P3，做）风险 4 错误语义不精确',
  '窗口存在但 `nextCursor === null`（A15「确实没有更早内容」）时报 `HistoryPageUnavailableError("not_negotiated")`，与「本连接未协商分页」共用一个 reason 与文案。要求：区分两种 reason 与文案，补测试。',
  '',
  '### S5（P3，做）风险 6 公开面缺少出口',
  '`historyOversize` 无读取方；`AWAITING_RESULT_STATUS`（`"awaiting_result"`）未从 `src/index.ts` 导出、README/code-index 未记录。要求：导出并记录（README + `docs/code-index/` 对应文件），让消费方能据常量判断状态。',
  '',
  RULES_COMMON,
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7c-sdk-fix.md：逐条 S1~S5 的「修了什么 / 证据（命令 + 结果）/ 未达成与原因 / 风险」；另列改动文件清单（绝对路径）与公开面变化（导出名、README 章节）。',
].join('\n')

const PROMPT_VERIFY_TUI = [
  '你是独立复核者（verification），默认只读、不采信实现者报告。',
  '',
  BRIEF,
  '',
  SHARED,
  '',
  '## 复核对象',
  '前一轮 TUI 复核（' + WORKTREE + '/.recon/S7-tui-verification.md）判定为 FAIL/PARTIAL 的条目在本轮修复后的真实状态。实现者报告在 ' + WORKTREE + '/.recon/S7c-tui-fix.md（当参考，不当代替）。',
  '',
  '## 逐条判据（每条给 PASS / FAIL / PARTIAL + 静态还是实测；实测要给命令与输出）',
  '1. D1（原 P1 FAIL）：**必须实测**空闲会话里滚动到已载入窗口顶端确实触发取页，并且触发路径不会被每帧/每次滚动放大成重复请求。实现者若无能失败的测试，你要自己补一个（标注为复核者新增）：先证明旧代码路径不触发（或给出旧行为的静态论证），再证明当前代码触发且只认领一次。',
  '2. D2：rewind 后迟到页确实被丢弃（构造「在途页 + rewind」的最小复现）。',
  '3. D3/D4/D5/D8 各自的最小复现是否已被堵住。',
  '4. D6（锚点）：实测 prepend 前后同一可见单元的屏幕行位置；若实现者标注为「仍 PARTIAL」，判定其理由是否成立（不要接受没有代价估计的放弃）。',
  '5. D7（文档）：`docs/code-index/peri-tui.md` 与本轮代码是否一致；`docs/verification/tui-integration-cases/README.md` 的约定是否被遵守。',
  '6. 回归：' + CD + CARGO + ' test --locked -p peri-tui --lib 全量（记录 passed/failed/ignored）；逐条核对既有断言有没有被放宽或删除（只读 `git diff`）；`' + CD + CARGO + ' check --locked -p peri-tui --all-targets`。',
  '7. 未协商 / 非分页连接的既有渲染路径是否被本轮改动影响（TUI 不协商时不应请求历史页、不应出现新字段）。',
  '注意：cargo 会争抢 target 目录锁，命令串行执行。',
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7c-tui-verification.md：判据表（PASS/FAIL/PARTIAL + 依据类型）、反例与最小复现、未验证项、结论（明确回答 REQ-C01 的「滚动触发」现在是否成立、A20 锚点是否达成）。',
].join('\n')

const PROMPT_VERIFY_SDK = [
  '你是独立复核者（verification），默认只读、不采信实现者报告。',
  '',
  BRIEF,
  '',
  SHARED,
  '',
  '## 复核对象',
  '前一轮 SDK 复核（' + WORKTREE + '/.recon/S7-sdk-verification.md）判定为 FAIL / 风险项的条目在本轮修复后的真实状态。实现者报告在 ' + WORKTREE + '/.recon/S7c-sdk-fix.md（当参考，不当代替）。',
  '',
  '## 逐条判据（每条给 PASS / FAIL / PARTIAL + 静态还是实测；实测要给命令与输出）',
  '1. S1（原 FAIL）：必须是**真实 Compact 失败**路径（不是 turn 失败）的实测——构造 `command_feedback` 的 error/cancel，断言失败展示出现且 `compactionStatus` 离开 `running`；并核对服务端确实会发这个形状（读服务端只读）。',
  '2. S2：初始回放事件在 `session.stream()` 与 docs 投影两条面上的行为是否自洽；若实现者选择记录行为变化，判定记录是否完整（README + code-index + 示例）。',
  '3. S3/S4/S5：逐条实测或静态核实（`test.failing` 是否已转正、reason 是否真的可区分、导出名与 README/code-index 是否一致）。',
  '4. A22 回归：代次暂存 / 一次安装 / 失败丢弃 / 重试无重复 / 订阅不漏消息——实跑既有用例并给出名字与结果。',
  '5. 回归：' + CD + 'bun run --cwd npm-packages/@peri-sdk typecheck、build、bun test（按仓库既有脚本名；记录 passed/failed，并区分**环境失败**与切片失败，环境失败要给单跑或对照证据）。',
  '6. 公开面：`src/index.ts` 导出、README、`docs/code-index/` 是否与本轮代码一致；有没有顺手删除既有导出。',
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7c-sdk-verification.md：判据表（PASS/FAIL/PARTIAL + 依据类型）、反例与最小复现、未验证项、结论（明确回答 A12 的「完成／失败状态正确」现在是否成立）。',
].join('\n')

phase('S7c 缺陷修复（TUI ∥ SDK）')
log('S7c TUI: trigger wiring, rewind invalidation, rpc timeout, span registration, anchor, docs')
log('S7c SDK: compact failure convergence, stream() behavior, block identity, error reasons, public surface')
const [tui, sdk] = await parallel([
  () => agent(PROMPT_TUI, { label: 's7c-tui-fix' }),
  () => agent(PROMPT_SDK, { label: 's7c-sdk-fix' }),
])

phase('S7c 独立复核')
const [verifyTui, verifySdk] = await parallel([
  () => agent(PROMPT_VERIFY_TUI, { label: 's7c-verify-tui' }),
  () => agent(PROMPT_VERIFY_SDK, { label: 's7c-verify-sdk' }),
])

return {
  s7c_tui_fix: tui,
  s7c_sdk_fix: sdk,
  verification_tui: verifyTui,
  verification_sdk: verifySdk,
}
