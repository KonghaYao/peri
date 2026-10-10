# P0：并发子 Agent 的工具行集中挂在最后一个调用下

状态：实现修复，等待现场验收。日期：2026-10-06。

## 观察与根因

用户截图中三个同名 code-reviewer 调用连续出现，九条子工具行集中显示在其后。截图足以证明可见归属错误，不能单凭截图断言存储正文也发生串写。

生产 `work_reason::prepare_invocation_intent` 为执行生成独立 invocation UUID，保留模型的 `tool_call_id`；`tool_dispatch/execution.rs` 在 durable dispatch 中把 `ToolContext.invocation_id` 改为执行 UUID。此前 SubAgentTool 将它放入配置及事件的 `parent_tool_call_id`，而 TUI `start_subagent` 用该字段匹配模型工具卡片。两种身份不相等，分组落入尾部待配对表，之后父卡片也无法认领；每组最多三条子工具摘要，三个相邻组在屏幕上表现为最后一个 Agent 下连续九行。

这不是需要再加一轮渲染缓存补丁的问题，而是执行领域与展示协议的身份边界错误。2026-10-05 的身份配对修复只贯通了字段名，没有验证生产身份来源；旧委派 fixture 将 invocation ID 与 tool-call ID 设成相同值，消费端测试也直接构造正确展示身份，导致各段测试分别通过但接线错误未被覆盖。

## 修复边界

- 子 Agent spawn/resume 配置改为 `parent_invocation_id`，继续用于原有委派授权、任务绑定、恢复元数据，不改变存储身份。
- 工厂根据可信父执行 intent 解析模型 `tool_call_id`，再填入现有启动事件 `parent_tool_call_id`。转换由同一 helper 负责，sync/background 与 spawn/resume 共用；不新增 wire 兼容字段，不把 invocation UUID 改成模型 ID。
- 共享委派 fixture 改用不同身份。实际工厂回归断言第一次启动与同一 child 的恢复各自对应新模型调用，覆盖同步及后台路径。
- TUI 真实 dispatcher → publication snapshot 回归覆盖三个同名 Agent、六种启动顺序、父卡片先到/后到、停止边界；每个 child 的工具只存在于自己的组，最终顺序必须为 card/group 交错。
- 原有 TUI 无父身份路径保持不变；此修复不宣称覆盖嵌套 Agent 的全深度路由或过期 occurrence 事件。

## 验证与验收

已新增回归并对旧身份赋值做可逆 mutation 验证：将事件身份改回 invocation ID 时，工厂回归实际失败（exit 101，收到执行 ID 而不是模型调用 ID）；还原后通过。未保留 mutation 改动。

2026-10-06，均从仓库根使用 `./scripts/cargo-rmcp-patched.sh` 执行：

- `test --locked -p peri-agent --lib subagent -- --test-threads=1`：125 passed，含 spawn/resume × sync/background 的模型调用身份回归，以及未登记身份在创建 child 前明确拒绝的回归。
- `test --locked -p peri-tui --lib -- --test-threads=1`：1560 passed、6 ignored。三组同名 Agent 各三条工具行，六种启动顺序与父卡片先到/后到共十二种排列均验证独立归属与停止边界。
- `test --locked -p peri-acp-types --lib event_v2 -- --test-threads=1`：56 passed。
- `test --locked -p peri-acp --lib mapper -- --test-threads=1`：37 passed。
- `test --locked -p peri-acp --lib test_production_stage_ -- --test-threads=1`：3 passed。最初的文件名过滤词 `executor_flow_dynamic` 命中 0 tests，未作为通过证据，已改用实际测试名复跑。
- `test --locked -p peri-agent -p peri-acp-types -p peri-acp --doc`：11 passed、2 ignored。
- `test --locked -p peri-middlewares --lib subagent::tool -- --test-threads=1`：31 passed、72 failed。在 `git archive HEAD` 的隔离源码目录重复同一命令，同为 31 passed、72 failed，失败测试名称集合完全一致；不能宣称该模块全绿，也未为本次修复改写这批夹具。
- 全量 `fmt --all -- --check` 被未改动的 `subagent/tool/tool_test.rs` 模块排序阻断；本次变更按文件格式化。本次范围 `git diff --check` 通过，工作树另有并行变更，未改动其空行等问题。
- `clippy --locked -p peri-agent -p peri-acp-types -p peri-middlewares -p peri-acp -p peri-tui --all-targets -- -D warnings` 在未修改的 `peri-model` 被 54 条 `result_large_err` 错误阻断，未宣称 lint 全绿。
- 全库文件规模扫描仍有 13 个既有测试超 1000 行；本次修改/新增源码及测试全部不超过 1000 行。
- `build --locked -p peri-tui`：通过，已生成工作区 `target/debug/peri`；未替换用户已运行的进程或全局安装。

现场验收：用修复后新构建的二进制并发启动三个同名 Agent，确认每个调用下仅出现自己的子工具行；再恢复同一 child，确认恢复调用与旧组保持独立。在该验收完成前保持 P0 未关闭，不把源码测试通过当成截图环境已修复。
