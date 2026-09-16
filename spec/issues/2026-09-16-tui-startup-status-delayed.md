# 新开 TUI 后 mode / model 等状态信息延迟出现

**状态**：Fixed
**优先级**：中
**类型**：性能 / 启动状态投影
**创建日期**：2026-09-16

## 问题描述

用户反馈 `agent-v3.15.0` 新开主界面时，界面已经出现，mode / model 等状态信息仍需约 1 秒才出现。场景是新会话启动，不是恢复历史会话。期望状态就绪后立即显示。

## 性能数据

在 macOS 本机，以 tag `agent-v3.15.0`（`8b894bebdf723e896926d115cac3ae5cdf89d94b`）独立 checkout 构建 debug binary，在 tmux 120×40 终端中观察真实 TUI。使用隔离 HOME、空工作目录、全新数据库、固定本地假 provider，不发送 prompt。每约 15–25 ms 抓取屏幕，记录首个非空画面和唯一模型文本 `startup-model` 出现的时间。

| 对照 | 首屏 → 模型显示 | 观察 |
| --- | --- | --- |
| 本机已安装的 3.14.5，2 次 | 0 ms / 0 ms（采样分辨率内） | 第一张非空画面已经有正确 mode / model |
| 3.15.0 tag debug，预热后 4 次 | 2048.8 / 2016.9 / 2041.6 / 2054.2 ms | 首屏约 36–65 ms，model 约 2.08 s 出现 |
| 3.15.0 + 边界日志，2 次 | 2061.4 / 2041.1 ms | 日志确认第一次快照被丢弃 |
| 仅将实验副本的周期由 2 s 改成 250 ms，3 次 | 430.8 / 370.0 / 312.2 ms | 长等待随周期缩短；仍未通过实验用 300 ms 门限，不代表修复完成 |

版本对照的构建 profile 不同，因此不据此推导总体性能倍数；关键因果证据是同一 tag、相同 debug profile 的单变量实验和边界日志。新链接二进制的首次进程启动还有额外冷启动时间，以上指标均以首屏为起点，不把它混入状态栏等待。

## 出现场景与复现

1. 新开 TUI，不恢复会话，不输入内容。
2. 首屏出现欢迎页及输入框，但状态栏 mode 暂为 `Don't Ask`，cwd / model 为空。
3. session 初始化完成后，界面仍等待一段时间；下一次快照刷新才显示配置中的 `Bypass`、cwd、model、effort。

临时诊断脚本和原始帧保存在 `/tmp/peri-startup-diagnosis/`，脚本为 `measure.py`。已执行：

```bash
python3 /tmp/peri-startup-diagnosis/measure.py \
  /Users/konghayao/code/ai/perihelion/target/debug/peri \
  --label tag315-confirm --repeat 2
```

执行时 binary 来自未改动的 3.15.0 tag，exit status 1；两次 gap 分别为 2041.6 / 2054.2 ms，均超过诊断门限 300 ms。该门限用于捕获本次秒级等待，不是产品已承诺的 SLA。诊断完成后已删除实验 worktree 和临时插桩，并重新构建当前工作分支的 debug binary（exit status 0）；复跑 tag 时须先构建对应 tag，不能仅凭 binary 路径判断版本。

## 诊断证据

`spawn_service_snapshot` 的首次 interval tick 立即开始，此后每 2 秒一次。`tick_once` 开始时捕获 `ACTIVE_EXECUTION_CWD`，接着异步查询线程列表、扫描文件及读取 session 信息。首次 session 初始化与它并发，成功时将执行目录从 `None` 发布为 `Some(effective_cwd)`。

`tick_once` 发布前检查发现目录身份变化，就丢弃整份快照并返回 `Ok(())`。调用方没有立即刷新信号，继续等下一个 2 秒 tick。因此已就绪的 mode / model / cwd 也随整份快照一起被延迟。

一组相对首次 snapshot tick 的实测日志：

- 0 ms：`snapshot tick start`。
- 207 ms：`initial session created`。
- 207 ms：`snapshot discarded`，仅 `cwd_changed=true`，session / scope / page 均未变化。
- 2001 ms：下一次 `snapshot tick start`。
- 2027 ms：`snapshot publish`。

250 ms 周期实验仍保留同一丢弃检查；下一次 tick 提前后，首个有效 snapshot 立即发布。这支持“丢弃后等待低频 tick”而非 TUI 已收到数据却漏刷新。检查是在 3.14.5 到 3.15.0 的 `8b894beb` 合并中引入；当前工作分支相关文件与该 tag 相同。

## 涉及文件

- `peri-tui/src/kit/service_snapshot.rs`：2 秒周期（78 行），初始目录捕获（179 行），过期检查及整份快照丢弃（337–342 行）。
- `peri-tui/src/acp_client/client/session.rs`：session 初始化完成后发布有效执行目录（161 行）。
- `peri-tui/src/kit/entry.rs`：并发启动 service snapshot 和首次 session 初始化。
- `peri-tui/src/kit/status_bar.rs`：从 SERVICE_SNAPSHOT 派生显示；初始空 mode 被映射成 `Don't Ask`。

## 后续修复方向

保留过期快照保护。在 session / active cwd 就绪或变化时通知快照任务立即刷新，并覆盖首次采样跨越 session commit 的确定性场景。CPU / 内存等低频轮询可继续保持 2 秒，避免单纯提高全量扫描频率增加空闲成本。初始状态应表达尚未就绪，避免临时显示与真实配置不同的权限模式。

初次调查仅诊断，修复及验证见下方记录。未验证其他平台、用户实际 release binary 或大型工作区；不把本机约 2 秒当作所有环境固定耗时。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-16 | — | Open | agent | 记录用户症状，完成 tag 复现与单变量诊断 |
| 2026-09-16 | Open | Fixed | agent | 执行目录投影完成后唤醒快照；确定性回归及真实 TUI 测量通过 |

## 修复记录

### 修复 #1（2026-09-16）

- **操作人**：agent
- **用户原意**：在本轮修复启动时 mode / model 等状态信息延迟出现的问题。
- **修复内容**：ACP client 的执行目录由既有共享值改为 watch 持有，不另建一份目录事实。new/load 发布完执行目录和 TUI 投影后通知快照任务；采样过程中发生的变化保留为待处理通知，因此旧快照被丢弃后立即重采。正常轮询仍为 2 秒，CPU/MEM 的内部 2 秒采样节流不变。shutdown 可以取消等待中的快照 RPC。初始空权限状态显示本地化“初始化中”，不再临时显示 `Don't Ask`。
- **涉及 commit**：见本文件的 Git 历史。
- **验证状态**：自动验证通过；待用户在实际 release 环境确认。

验证证据：

1. 新增 3 个暂停时钟回归测试，修复前全部失败，修复后通过：首次采样跨越 session commit 被丢弃、首份快照发布后才建立 session、退出时取消未完成查询。前两个场景在 2 秒周期尚未到达时断言刷新；另断言更新结束后回到低频轮询。测试复用真实 ACP client 与 mpsc transport，在协议边界提供受控响应，不访问用户 HOME。
2. `cargo test -p peri-tui --lib -- kit::service_snapshot::tests`：18 passed，exit 0。
3. `cargo test -p peri-tui --lib`：1569 passed，2 ignored，exit 0；记录在 `/tmp/peri-startup-diagnosis/peri-tui-lib.log`。
4. `cargo test -p peri-tui --doc`：执行成功，无 doc test 用例。`cargo build --workspace`：exit 0；保留已有 macOS linker 的 unwind section 警告。
5. `cargo clippy -p peri-tui --all-targets -- -D warnings`：本工作区第一次执行被并发进行的历史会话迁移改动阻断。随后在 `d86fff1a1` 隔离工作树仅应用本次 TUI patch，以独立 target 目录执行相同 lint，exit 0。未修改其他任务的代码，验证后已删除隔离工作树；记录在 `/tmp/peri-startup-diagnosis/clippy-isolated.log`。
6. 真实 tmux TUI 验证：仍使用固定 provider、隔离 HOME、空目录和 300 ms 诊断门限。首次与编译并行的一组有 1 次未捕获到首屏（无日志，无法判断原因），1 次 357.0 ms，3 次 203.9–252.5 ms；保留原始失败记录，不把这一组计为全通过。编译结束后的连续 5 次测量分别为 **134.3 / 223.8 / 249.9 / 232.8 / 0.0 ms**，全部通过，exit 0。0.0 ms 表示采样分辨率内首屏已有模型文本，不代表零初始化成本。原始记录在 `/tmp/peri-startup-diagnosis/startup-fixed-confirm-*`。

最终真实终端验证命令：

```bash
python3 /tmp/peri-startup-diagnosis/measure.py \
  /Users/konghayao/code/ai/perihelion/target/debug/peri \
  --label startup-fixed-confirm --repeat 5
```
