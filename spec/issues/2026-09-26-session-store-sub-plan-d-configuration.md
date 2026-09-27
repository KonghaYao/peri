# 会话资源拆分 — 子计划 D：定位、凭证与部署装配

> 状态：设计计划，未实施。日期：2026-09-26。
> 上级：[总计划](2026-09-26-session-store-plan.md)。依赖：[A](2026-09-26-session-store-sub-plan-a-contracts.md)、[B](2026-09-26-session-store-sub-plan-b-local.md)、[C](2026-09-26-session-store-sub-plan-c-turso.md)。消费迁移见 [E](2026-09-26-session-store-sub-plan-e-consumers.md)，测试见 [F](2026-09-26-session-store-sub-plan-f-verification.md)。

## 1. 目标

打开入口表达「会话存储在哪里、如何访问」，而不是「SQLite 文件在哪里」。Resources 负责唯一后端装配；TUI、print、ACP stdio、meta 不各自解释远程 SDK 或降级。

保持默认本机行为。只读元数据命令继续在 provider/settings/Agent 初始化前执行，不因为引入远程而加载所有配置、启动执行资源或创建本机执行登记。

## 2. 现有入口与迁移清单

| 当前文件/符号 | 现行职责 | 本次变更 |
| --- | --- | --- |
| `peri-resources/src/context.rs::Resources::{open,open_with}` | `Option<PathBuf>` → SQLite，写打开失败尝试只读 | 归一解析后的 open request，选择 adapter 与本机执行组合 |
| `peri-resources/src/sessions/mod.rs::open_thread_store_read_only` | 独立 SQLite 只读 seam | 转入同一选择器的明确只读模式，不走写打开再降级 |
| `peri-agent/src/resources.rs` | 资源打开薄入口（`open_thread_store{,_with}`） | 更名 `open_session_resources{,_with}`，返回 `Arc<dyn SessionResources>`（A §2.1）；只保留一个入口，不再选择后端 |
| `peri-tui/src/main.rs` | `--db-path`/`dbPath`、meta 早路由 | 新定位参数、冲突验证、受限 meta grammar 同步 |
| `peri-tui/src/{launch.rs,app/mod.rs,cli_print.rs}` | 三条部署参数传递与打开；`App::new(db_path)` 与 `app.services.thread_store` | 传同一 open request，不重新读取环境决定不同存储；services 持有 `Arc<dyn SessionResources>` |
| `peri-tui/src/cli_meta.rs::run_meta_session` | UUID 校验后只读打开、九字段 DTO（`SessionMetaDtoV1`） | 保留早校验与 DTO；使用统一只读入口与安全错误映射；只读不写任何本机文件 |
| `peri-acp/src/host/stdio/mod.rs::StdioInput` | stdio 的 `cwd`/`permission_mode`/`db_path` 三个字段 | 携带同一资源定位描述，装配时只解析一次 |
| `peri-tui/src/thread/mod.rs`、`peri-agent/src/thread/mod.rs` | 具体 SQLite 类型 re-export（`SqliteThreadStore`/`FilesystemThreadStore`/`open_thread_store_read_only`） | 生产导出删除（A §7.1）；测试迁至明确资源测试入口 |

上述是路径清单，不表示 TUI 可以直驱执行；交互仍经 ACP。

## 3. 建议参数与解析决策

### 3.1 最小配置面

拟定公开输入：

- `--session-store <locator>`：本机路径、已确认引擎的远程 locator（Turso Cloud 测试库），或 `env:<变量名>` 间接定位。远程引擎由 C-01 对目标库只读探测后选定（Turso 引擎 / libSQL 引擎两条候选，C §5.0）；embedded replica 与 sync 不在首期支持面内，出现时按 Unsupported 报错，不当作另一种 locator 解释。
- `--session-store-token-env <变量名>`：只表达凭证来源，不接受 token 字面量；远程模式必需。是否为它设一个统一默认名仍未决（G-03），在决定前不得内置任何默认或候选名（§6.1）。
- 既有 `--db-path` 和 `--dbPath` 保留本机兼容性；与 `--session-store` 同时出现直接参数错误，不设隐式覆盖顺序。
- 都未提供时仍打开默认本机 `threads.db`。仅设置 `TURSO_URL` 不自动切换后端，避免意外上传。

使用已有环境变量的拟定调用形态是 `--session-store env:TURSO_URL --session-store-token-env TURSO_TOEKN`；这是计划示意，不在本轮执行。变量拼写未确认（G-03）：实现不得为它添加别名、同义词或“先试 A 再试 B”的回退，也不得把 `TURSO_TOKEN` 当作隐式同义。

第一版不新增完整 settings profile 系统。typed `SessionStoreOpenRequest` 位于资源装配层，包含解析后的 locator、凭证来源、访问意图；凭证对象不实现泄密的 Debug/Serialize。跨 crate 仅传与职责相符的中性类型，不把 SDK 类型放进 `peri-acp-types`。

### 3.2 解析顺序

1. 先验证 CLI grammar、互斥选项；meta 先验证 session UUID。
2. 解析 locator：本地路径保留 Windows drive/UNC 语义，不能将 `C:` 当 URI scheme；`env:` 仅解引用一次且不递归。
3. 只在选择远程后读取所指定的 URL/token 环境变量；空值、缺失、非法 URL 各有安全类型化错误。
4. URL 禁止 userinfo 与携带凭证的 query；禁止日志输出完整配置。协议与引擎对应按 C-01 证据校验；仅有 `https` 不能可靠辨识引擎时明确要求显式选择，不猜测或轮流试两种 SDK。**`libsql://` 同样不能辨识引擎**（C §2 review-3：选定远程驱动的 remote URL 示例就是 `libsql://…`，libSQL 与 Turso 两种引擎都可能出现该 scheme），因此引擎只能由显式选择或已确认的 locator 语法给出；实现不得以 scheme、端口或响应探测推断引擎。
5. 普通读写和显式只读使用同一选择结果进入 Resources factory。配置解析不是连接成功，更不是 schema 可写证明。
6. 启动结果携带实际访问模式/行为能力；后续恢复到另一个 cwd 不能重新解析出另一个存储。

若 C-01 确认必须支持无引擎信息的 HTTPS URL，再增最小 `--session-store-engine <libsql|turso>` 参数，与原生 scheme 冲突时报错；没有证据前不扩大配置面。

## 4. 凭证与 dotenv

- 用户提供 `.env` 中已有 `TURSO_URL` 和 `TURSO_TOEKN`；计划不读取文件，不确认其值/引擎/有效性，不修改拼写。
- 资源库只接受显式注入的凭证，不自行搜索 cwd 或父目录 `.env`；避免恢复会话时换目录悄悄换账号。
- 云实验 runner 在明确给出的 `.env` 路径上使用 dotenv parser，只取所需键并注入受控测试子进程，不使用 shell `source`/`eval`，不 dump 整份环境。
- 不因 meta 需要云凭证而开启常规 provider 配置加载；process env 和受限 locator 参数足够。产品若已有启动 dotenv 加载，保持其既有责任，不在 adapter 再加载一次。
- 配置、错误链、SDK Debug、HTTP tracing 和测试快照不得输出 token、带凭证 URL 或原始响应正文；安全诊断使用行为名、错误种类和非敏感运行标识。
- 不创建或提交真实 `.env`/token fixture；测试凭证值使用运行时生成的合成哨兵，仅用于检测不泄漏，禁止复制真实密钥。

## 5. 打开与只读语义

| 请求/故障 | 决策 |
| --- | --- |
| 默认或指定 SQLite 读写 | 保留现有 schema 检查、可恢复写打开失败后的只读尝试；两次都失败保持原错误意义 |
| 未知 SQLite schema | 保留现有版本/列形状判定细节，不简化为全部拒读或全部降级 |
| 显式 SQLite 只读 | 不创建目录、库、锁或迁移 schema |
| Turso 读写 | 先核实读写权限/行为可用性，再装配；不自动切回 SQLite |
| Turso 明确只读权限 | 可提供历史读取，执行/新建在副作用前拒绝；不得用普通写失败推导「只读可用」 |
| 鉴权失败/网络失败 | 保留安全分类与错误，不当空库、不初始化替代数据库 |
| 显式 Turso 只读/meta | 不发任何写请求、schema 初始化、installation 登记或 owner 获取；仅检查读取兼容性。**只读允许读取 schema/StoreId/binding**，但读不到 ≠ 空库可初始化 |
| 远程模式下本机 registry 库不可写 | 需要锚点的写/执行直接报错；**不允许**把只读请求静默降级成“远程只读会话”，也不为本机库另造降级路径 |
| 本机执行 registry 缺失 | 历史读取独立成立；写入/执行按 B 阻塞，不自动重建原绑定 |

完整链（locator → 远端 schema → StoreId → 本机登记 → binding → workspace 证据 → owner）的状态/读写矩阵见 [B §6.1](2026-09-26-session-store-sub-plan-b-local.md)；D 只负责第 1 环的解析与把 `AccessMode` 传给 factory。

`peri meta session` 的现有九字段 allowlist 与本机错误/退出码保持；新增远程错误映射应有明确测试。没有 wire 版本需求时不改变现有成功 DTO。错误至少区分缺配置/无权限/不可用/不兼容/目标会话不存在，不能统一回报数据库不存在。

## 6. Factory 与生命周期

Resources 持有一个稳定会话资源门面 `Arc<dyn SessionResources>`（A §2.1）；SQLite factory 装配共享 pool 的数据 adapter/本机执行实现；Turso factory 装配远程数据 adapter、StoreId 和本机执行状态。消费侧不能取得裸数据写口，也不能取得具体 adapter 类型（`Resources::session_resources()` 是唯一访问器，`thread_store()` 随迁移删除）。

部署资源关闭应有唯一关闭 owner，clone 的业务门面不能随意关闭全局连接。先会话排空、确认持久化，再关 adapter worker/连接；`Drop` 只作兜底释放，不代替已完成证据。现有 Runtime 不持久化资源事实，不在 Runtime 另开连接/注册表。

D 只负责构造/持有关系和参数传递；会话 close 的生命周期行为归 B/E，远程收敛归 C。

### 6.1 云授权状态（2026-09-26 本轮更新）

用户已确认 `.env` 指向**测试库**，授权只读探测、初始化本任务 schema、合成数据与只清理本轮对象；
原先记录的 `cloudAuthorized=false` 由此失效（真实云验收不再因此 blocked）。凭证变量名拼写已由
只读键名报告确认（`TURSO_URL` / `TURSO_TOEKN`）；实现仍不内置默认名、别名或候选名。

D-01–D-03 已落：typed `SessionStoreOpenRequest`、locator 纯解析（本机路径、`env:` 引用、远程端点；
引擎只由已确认语法或显式选择给出）、`Resources` 作为唯一后端选择点；显式只读走独立
只读 seam（不建目录/库/锁、不迁移 schema）；远程 locator 在 adapter 落地前返回类型化
`RemoteStoreNotWired`，不降级为本机库。**D-04 已落（同日第三轮，见下）**；**D-05 未做**：关闭 owner
与协议映射（E 联调）仍待实施，真实云仍未实跑。

第四轮（同日）复核：D-01–D-04 的落盘在资源/CLI/TUI（含 meta、print）/ACP stdio 的受影响测试目标上
复核通过（命令与结果见母需求 §9.7）。两处遗留如实记录、不在 D 面临时补：①
`Resources::open_with(Option<PathBuf>)` 与 `peri_agent::resources::open_session_resources{,_with}`
在生产已无消费方（调用方只剩测试），生产面统一走 `open_deployment`，归一或删除随下一批；
② §8 验收 4 的「显式只读对两后端均无写副作用」目前只对本机后端成立，远程只读要等 C-02/C-03
落地后才有对象可验。

同日第二轮补齐 D-01–D-03 的接口与回归面（仍未做 D-04/D-05）：

- 类型命名落定：`StorageLocator`（未解析输入：默认 / 本机路径 / 原文 / `env:` 引用）与
  `SessionStoreOpenRequest`（locator + 引擎 + 凭证来源 + 访问意图）。
- `AccessIntent` 纯解析：只接受 `read-write` / `read-only` 两条拼写，无别名、无大小写模糊匹配；
  解析失败不回显原始取值。`Resources::open_locator(locator, engine, credential_env, access)`
  在进入任何 I/O 之前完成全部纯解析（拼写 / 引擎名 / locator 形态冲突）。
- 环境变量读取面固定为两处：`env:` 形式的 locator 与远程 adapter 取凭证值。仅设置云 URL/token
  变量不切换后端，也不使默认本机库变成远程库（纯逻辑 + 打开层回归各一条）。
- 显式只读失败保持类型化：`ReadOnlyThreadStoreError` 留在 source chain（消费侧可按 kind 映射
  退出码），路径只加在 context 上；只读打开与普通打开共享同一选择点，不新增文件、不登记 owner。
- 远程 locator 返回类型化 `RemoteStoreNotWired { engine }`，并注明这是 **C-02/C-03 adapter 落地前的
  临时状态**：既不静默回落本机库，也不把「连上了」当成「可用了」。

同一日第三轮落 D-04（部署参数迁移）：

- 跨层中性类型 `peri_acp_types::session_store::SessionStoreDeployment`（locator 原文 / 可选引擎名 /
  凭证**来源** / `AccessMode`，`Debug` 不回显 locator 原文与凭证变量名）：部署入口只传它，不传
  `Option<PathBuf>`，也不传 SDK 或凭证值。
- `Resources::open_deployment(&SessionStoreDeployment)` 是各入口唯一的装配点：进入任何 I/O 前把部署
  参数一次性转成 typed open request（`SessionStoreOpenRequest::from_deployment`），再进唯一选择点。
  取代并删除第二轮的 `Resources::open_locator`；`AccessIntent` 改为由 `AccessMode` 单向映射
  （部署面无拼写输入，`AccessIntent::parse`/`AccessIntentError` 随之删除，不留死接口）。
- CLI：`--session-store`（含 `--sessionStore`）、`--session-store-token-env`、`--session-store-engine`
  遵循 D1 规则；`--db-path`/`--dbPath` 保留。两个定位入口互斥由 `validate_cli` 判定（错误早于任何
  I/O，且部署参数构造同样拒绝，不设隐式覆盖顺序）。meta 的受限 grammar 同步为只接受定位参数。
- 入口接线：TUI `main`/`TuiOptions`/`launch`/`App`、`-p` print、ACP `StdioInput.session_store` 与
  `peri_agent::resources::open_session_resources_deployment`、`peri meta session` 早启动都传同一份
  定位描述；`App` 持有门面后，`attach_acp` 与恢复会话不再重新解析存储。`peri_tui::thread` 对
  消费侧的独立只读 seam re-export 一并删除。
- meta 错误面：新增公开 `StoreOpenFailure` + `classify_open_failure`（按类型化 source chain 分类，
  不解析文本、不回显 locator/凭证）；`store_not_configured`(2) 与 `store_unavailable`(4) 两个新 kind，
  缺库仍是 `database_not_found`(3)，不统一回报成「数据库不存在」。

验证证据（本机离线；隔离 HOME；未实跑云、未新增 `#[allow]`/`#[ignore]`）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 233 passed / 0 failed（`sessions::open_tests` 17 条） |
| `cargo test -p peri-tui --lib` | 1671 passed / 0 failed |
| `cargo test -p peri-tui --bin peri` | 85 passed / 0 failed（含新 CLI/meta 回归） |
| `cargo test -p peri-tui --test print_exit` | 9 passed / 0 failed（`--db-path` 兼容未退化） |
| `cargo test -p peri-acp --lib host::stdio` | 17 passed / 0 failed |
| `cargo check --workspace --all-targets` / `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

真实二进制端到端（隔离 `HOME`/临时目录，只读、无网络）：缺库 → exit 3 `database_not_found`；
`--db-path` 与 `--session-store` 同给 → exit 2 `invalid_argument`；远程 locator → exit 4
`store_unavailable`（输出不含 locator 原文）；非法 UUID → exit 2 `invalid_session_id`；四条命令后
临时目录仍为空（只读入口不建目录/库/侧车）。

第二轮遗留的接口描述（`open_locator` 与 `AccessIntent` 拼写解析）已被本轮取代。

## 7. 任务分解

- D-01：列全 `db_path`/`open_thread_store*` 的生产路径，建立解析输入与 typed open request。
- D-02：实现 locator 纯解析、互斥、路径/URI/env 引用和凭证防泄漏测试。
- D-03：统一 Resources 普通/只读选择；本机兼容入口仅归一转发。
- D-04：迁移 TUI/print/stdio/meta/Agent resource wrapper 的部署参数；不留独立 SDK 打开路径。
- D-05：集成关闭 owner、访问模式和安全诊断；与 E 的 protocol mapping 联调。

## 8. 验收

1. 相同 locator 从所有部署入口得到相同 StoreId/访问模式；默认路径仍本地且不访问网络。
2. `--db-path` 与新参数冲突早于 I/O；非法 meta UUID 不读凭证、不连接、不创建文件。
3. Unix 相对/绝对路径、Windows drive/UNC、空 env、递归 env、带凭证 URL 均有测试。
4. 显式只读对两后端均无写副作用；本机读失败语义与既有测试一致。
5. metadata DTO/退出码与安全错误检查覆盖人类输出和 JSON；不加入 frozen/消息正文/连接配置。
6. 原始 token 和 URL userinfo 不出现在 Debug、错误或追踪；凭证缺失不被自动生成或 fallback 掩盖。

命令与测试安装位置由 F 统一登记；本计划不宣称任何拟新增 CLI 已存在。
