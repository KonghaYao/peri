# Worktree 会话身份与执行归属

**状态**：Verify — 实现与本机自动验收完成；仅保留其他平台运行和人工交互验收。
**优先级**：高
**创建日期**：2026-09-12
**事实源**：[身份设计](../../docs/design/session-workspace-identity.md)、`ARC-WORKSPACE-001`。

## 已确认范围

> 2026-09-16 兼容性范围调整：旧历史发现、只读预览与按保存目录恢复由
> [3.15 history P0 修复](2026-09-16-p0-315-history-inaccessible.md) 接续。
> 下述“不回填绑定”仍适用于开库阶段；显式恢复时允许原子接纳旧根会话。

用户要求直接实施，并明确旧历史无需迁移；随后明确沿用单库
`~/.peri/threads/threads.db`。已知旧 schema 在事务中补列/表，旧历史行保持原样，
不回填绑定。新会话使用同一数据库且必须绑定；未知 schema / 版本在写入前拒绝。
升级前停止旧版进程，不支持新旧二进制混用同库。

项目列表聚合同一 Git common directory 的 worktrees；每个 checkout 有独立
WorkspaceId。恢复始终使用保存的工作区及相对目录，启动 cwd 不能覆盖它。
`-c` 按当前工作区及子目录查询，`-r` 按选定会话恢复。普通 fork 保持原环境。

不实现 worktree 创建/删除、移动后重关联、跨工作区复制续作、跨机器同步和实时
attach。丢失或被替换的目录保留历史，但不自动执行；崩溃后未确认清理的记录返回
RecoveryRequired，不用超时或 PID 缺失假定已经安全收尾。

## 原始故障

原实现 `ThreadMeta.cwd` 同时承担列表归属和执行定位：TUI 按启动 cwd 查历史，
冷 load 从请求 cwd 构造 SessionState，却恢复原 frozen；热 load 保留已有 cwd。
此外宿主的 hooks、插件、MCP 和项目配置在启动时装配，单独修正 SessionState.cwd
无法保证实际工具环境正确。

## 实施内容

- Resources：ProjectId/WorkspaceId 登记、文件对象证据、绑定校验、SQL scope 与
  cursor 分页、只读打开、执行 OS lease 和 dirty generation。绑定写入与 clean
  交接共用 gate；取消中的未知数据库写入不能被标 clean。
- ACP：new/load/resume/fork、prompt、Workflow 的执行准入；per-session 环境装配，
  frozen 与身份响应在发布前准备；关闭未完成保留实际资源和 owner。
- Agent：TaskManager 独立保存执行排空证据；Bash 前后台交接、取消和子会话状态
  收尾纳入 owner。绑定子会话恢复必须属于同一根会话；同根兄弟可恢复。
- Process：Bash、hooks、MCP、LSP 与 JS 共用 OS owner。Windows 挂起后保存精确
  process handle 并入 Job 才执行；Unix 证明所属进程组退出，显式脱组不在保证内。
- MCP/LSP：实际子进程使用保存目录；static init/reconnect 与 Dynamic 共用 owner。
  协议 close、child 与 stderr 的唯一 owner 跨取消/超时保留，重试等待同次关闭。
- Hooks/PTC：异步和独立 compact hooks、PTC 的真实执行归 TaskManager；PTC
  原生 Node fs 跟随会话 cwd。SessionEnd 随 ACP 会话执行；目录已失效时跳过
  未开始的 hook，继续资源关闭。Cron 使用实际会话权限。
- Workflow：运行、恢复复用同一登记与完成路径，关闭后拒绝新执行；runner 等待 JS
  与子 Agent 收尾；实际执行终结与 Defer/UI 完成通知分别结算。
- TUI：协商 `peri.sessionWorkspaceV1`，项目/工作区列表经 ACP 获取；成功恢复后
  更新 active cwd、文件和服务视图；失败保持明确阻塞，不自动新建并发送 prompt。
  本地配置面板明确编辑宿主配置及其文件路径，会话模型/权限通过会话 RPC 修改。

## 验收矩阵

| 场景 | 必须观察到的结果 |
| --- | --- |
| 主树、linked worktree、子目录、symlink | 同项目、不同工作区，保持准确执行目录 |
| 独立 clone、branch 变化、nested repo | 不按 remote 或 branch 猜测归属 |
| 目录删除/重建/move、Git 拒绝或不可用 | 明确错误，历史不被改绑 |
| 热/冷 load、resume、fork、`-r` | 精确恢复 binding/frozen；错误 cwd 不发布 active 状态 |
| `-c`、分页与刷新 | 精确工作区/子目录；项目列表完整；查询错误不同于空结果 |
| 不同工作区的 hooks/plugins/MCP/权限 | 资源及权限消费会话环境，陈旧 UI 响应不覆盖当前会话 |
| 多真实进程竞争同一会话 | 同时至多一个 owner，clean 后才可接管 |
| 崩溃、写入被取消、关闭 Incomplete | dirty 保留，拒绝自动再执行 |
| 已登记写入与 close 交错 | SQL 终态早于 clean 与 OS 锁释放 |
| shell/hooks/PTC/子 Agent/Workflow 取消 | 请求取消不等于执行终结；资源实际排空后才释放 lease |
| 首次并发开库、旧库写打开 | 初始化/升级序列化；旧历史行不变，新会话绑定，DDL 失败完整回滚 |
| MCP/LSP 主进程先退、关闭被取消 | 后代和协议任务保留实际 owner，未排空不宣称 Complete |
| Windows | 源码交叉编译及本机平台运行结果分开报告 |

## 验证记录

- 完整 TUI lib：1559 passed，2 个既有 ignored；CLI metadata：19 passed；
  CLI print/退出：5 passed。覆盖初始化失败关闭会话、恢复草稿且不发送输入，
  当时验证默认库选择；单库修正后的验证见下方补充。
- 真实 PTC：18 passed，覆盖 A/B 原生相对文件访问、调用方 abort 后会话清理、
  关闭后拒绝新执行。
- 统一完整 lib：ACP 648、acp-types 398、Agent 767、Resources 88、Middlewares
  1646、LSP 96、Process 2、JS 50、Workflow 96 项通过；Middlewares 4 项和
  Workflow 3 项既有 ignored 保持不变。没有失败项。
- `cargo clippy --workspace --all-targets -- -D warnings` 通过；
  `cargo test --workspace --doc` 通过（8 passed、3 个既有 ignored）。
  完整日志：`/tmp/peri-worktree-libs-final.log`、`/tmp/peri-worktree-tui-final.log`、
  `/tmp/peri-worktree-clippy-final.log`、`/tmp/peri-worktree-doctests-final.log`。
- Windows ProcessTree、JS 含 tests 已通过 MSVC target 交叉编译，资源身份实现亦
  通过独立 Windows harness 编译；LSP verbatim drive/UNC 经过纯函数测试。
  这些不替代 Windows 主机运行验证。
- fmt、typos、依赖方向、变更文档本地链接和最终 diff 检查通过。
- 最后将 SessionEnd 任务构造归回 ACP 装配层后，worktree 定向回归 9 项通过；
  workspace clippy、ACP doc tests 和依赖检查再次通过。日志：
  `/tmp/peri-worktree-sessionend-boundary.log`。

## 单库修正（2026-09-12）

- 按用户要求恢复读写/CLI 默认 `threads.db`，v2 仅表示 schema 版本。
- 已知旧表在事务中补列并创建身份表，原行和上下文字节保持不变；旧会话不回填
  绑定、不获得执行权。未知 schema / 未来版本仍在写入前拒绝。
- 补充旧版完整/早期 schema、同库新旧会话共存、并发升级、失败回滚、未来版本
  拒绝与独立进程默认 writer / CLI 路径回归。
- `cargo test -p peri-resources --lib`：95 passed；
  `cargo test -p peri-tui --test meta_session_cli`：19 passed；
  `cargo test -p peri-tui --test print_exit`：5 passed；
  `cargo test -p peri-acp --lib worktree`：9 passed。print fixture 首次被沙箱禁止
  localhost 监听，允许本地 mock 服务后通过；所有数据均位于临时目录。
  日志：`/tmp/peri-single-db-resources.log`、`/tmp/peri-single-db-cli.log`（metadata
  通过及首次 print 沙箱失败）、`/tmp/peri-single-db-print.log`、
  `/tmp/peri-single-db-acp.log`。
- workspace 全目标 clippy（`-D warnings`）通过；Resources / Agent / acp-types
  doc tests 通过（8 passed、2 个既有 ignored）。fmt、diff、typos、18 条依赖规则
  和 31 个本地文档链接检查通过。日志：`/tmp/peri-single-db-clippy.log`、
  `/tmp/peri-single-db-doc.log`。本轮未打开或修改用户实际数据库，未提交 Git。

## 冗余收敛（2026-09-12）

- 按审查结论取消 binding `revision` 的持久化，协议兼容字段仍返回常量 1；
  既有 schema 2 原地升级到 3，保留会话绑定与执行状态，继续使用 `threads.db`。
- 新库与旧库补列共享列定义，读写默认路径收口为纯路径函数；只读入口不建库或升级。
- Project / Workspace 关系、身份快照、索引和执行代次保留；既有缓存清理单独评估。
- `cargo test -p peri-resources -p peri-acp-types --lib`：Resources 100、acp-types
  398 项通过；`cargo test -p peri-tui --test meta_session_cli`：19 项通过。
  加强 DROP 失败错误断言后，目标回滚测试再次运行，1 项通过。
- workspace 全目标 clippy（`-D warnings`）、fmt、diff、typos、18 条依赖规则、
  变更文档本地链接检查通过。Resources / acp-types doc tests 命令正常退出，
  其中没有可执行用例，2 个既有示例保持 ignored，不作为运行覆盖证据。
- 日志：`/tmp/peri-db-redundancy-libs.log`、`/tmp/peri-db-redundancy-cli.log`、
  `/tmp/peri-db-redundancy-clippy.log`、`/tmp/peri-db-redundancy-doc.log`、
  `/tmp/peri-db-redundancy-rollback.log`。三个 subagent 完成修改及交叉审查；
  HOME 隔离的默认路径测试限定 Unix，Windows 使用系统 known-folder API，
  不通过 HOME/USERPROFILE 覆盖测试位置。未打开实际用户数据库，未提交 Git。

## 实际旧库启动回归（2026-09-12）

- 用户运行 `./dev.sh` 被 `UnsupportedDatabaseSchema` 阻断。只读检查实际 DDL
  确认 user_version 为 0，除 threads/messages 外还保留 thread_goals；此前检查
  错误要求全库恰好两张表，合成测试遗漏了这一历史结构。
- 修正为验证必需真实表与列，允许并保留其他业务表；不硬编码 thread_goals 白名单。
  从实际库导出无用户行数据的 `fixtures/legacy_with_goals.sql`，通过 Resources
  启动入口验证目标、消息、旧索引和额外扩展表不变，且新会话仍可绑定/写入。
- 回归测试先复现同一启动错误（`/tmp/peri-legacy-goals-red.log`）；补充同名 VIEW
  不得替代必需表的拒绝路径。
- Resources 102 个测试通过；`cargo build -p peri-tui` 与 workspace 全目标
  Clippy（`-D warnings`）通过。重新构建的真实 CLI 在隔离 HOME 下使用实际旧库
  DDL 与合成数据，走默认数据库路径、标准 print 模式和本地 mock provider，完成
  启动、新会话持久化、模型响应与退出（exit 0）；旧目标记录与外键完整性保持。
- 日志：`/tmp/peri-legacy-goals-final.log`、`/tmp/peri-legacy-goals-build.log`、
  `/tmp/peri-legacy-goals-clippy.log`；进程验证脚本：
  `/tmp/peri-legacy-goals-print-smoke.py`。实际用户库仅只读检查 DDL，未写入。
  PTY 探针观察到 TUI 进入 alternate screen，但未验证正常交互退出，因此不算
  交互式 TUI 验收证据。

## 仍需平台验收

- 在 Windows 与 Linux 主机验证真实跨进程 lease、Git worktree 文件身份、
  shell/MCP/LSP/JS 取消与退出生命周期；本轮仅在 macOS 运行这些场景。
- 交互式 TUI 工作区切换与真实 OAuth 授权流程未做人工体验验收；协议、snapshot、
  load/cancel 和 mock 服务路径已由自动测试覆盖。

本 issue 只保留尚未取得的验收证据；已落地的稳定契约见身份设计与代码索引。
