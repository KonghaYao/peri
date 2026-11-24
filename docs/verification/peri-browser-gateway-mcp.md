# peri-browser-gateway MCP 合并验收记录

> 基线：`local/peri-browser-check`，`1d97f3f223` 起本分支的提交（`42ef2e669` 合并、`860df7e5e` 文档、`bc04e8064` 页面 owner 修复、`249fe5ae6` 引擎断开视为已释放、`eae68a04d` 冒烟记录），以及与本记录同批提交的单一二进制 CLI、安装脚本与发布通道改动；2026-11-23。平台 macOS 26.6.2 arm64，Rust 1.99.0，patched rmcp 3.9.0、tokio 1.53.1、axum 0.8.9。功能行见 [F-EXT-016](../reference/feature-catalog.md)，运行时契约见 [gateway README](../../mcp-packages/peri-browser-gateway/README.md)。

## 范围与证据性质

合并目标：`peri-browser-gateway` 成为唯一浏览器服务，在同一 loopback listener 上提供 MCP Streamable HTTP 与 monitor；每个 MCP 会话独立 `BrowserCore`；页面写入只有一条 `PageControl` 路径；`peri-browser` 的 stdio 二进制与 UDS 协议退役。

实现由工作流 agent 按 `.peri/plans/gateway-mcp-merge-plan.md` 产出。三次运行均在收尾前被模型 provider 基础设施错误打断（API Key 查询失败、DNS 解析失败、TLS `unexpected EOF`），没有 agent 产出的实现报告或独立验证报告。剩余测试修正、lint、文档与下列命令由协调者在本机完成。**因此本记录是协调者运行的证据，不是独立复验。**

## 命令与结果

环境：

```bash
export MISE_PYTHON_VERSION=3.12.15
export TC=/Users/mino/.rustup/toolchains/1.99.0-aarch64-apple-darwin
export CARGO_HOME=/tmp/cargo-home-rsproxy
export PATH="/opt/homebrew/bin:/Users/mino/.local/share/mise/installs/python/3.12/bin:$TC/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.99.0
export CARGO_TARGET_DIR=/tmp/peri-browser-target
export CARGO_INCREMENTAL=0
```

本机 `cargo` 会打印 `ignoring the configured target directory ... this cargo always uses ./target`：上面的 `CARGO_TARGET_DIR` 在此环境不生效，产物落在仓库 `./target`；测试输出仍显示本次修改后的 crate 被重新编译。

| 检查 | 命令 | 观察结果 |
| --- | --- | --- |
| 构建 | `./scripts/cargo-rmcp-patched.sh build --offline --locked -p peri-browser-gateway --bin peri-browser-gateway` | PASS，`Finished dev profile`。 |
| 测试 | `./scripts/cargo-rmcp-patched.sh test --offline --locked -p peri-browser-gateway` | PASS：lib 124，`src/main.rs` 0，`tests/mcp_http` 19，doc 0。19 项由 3 个 transport 级用例与 16 个契约模块用例组成（`monitor_handoff` 7、`shutdown` 5、`session_isolation` 2、`onboarding_wire` 1、`process` 1）。 |
| Clippy | `./scripts/cargo-rmcp-patched.sh clippy --offline --locked -p peri-browser-gateway --all-targets -- -D warnings` | PASS，`CLIPPY_EXIT=0`。 |
| 格式 | `cargo fmt -p peri-browser-gateway -- --check` | PASS。 |
| 依赖门 | `bash scripts/check-layer-imports.sh` | PASS：22 条规则，违规边 0。 |
| 文件大小 | `bash scripts/check-file-size.sh` | **未运行**：脚本要求 `ripgrep`，本机未安装。手工对本次新增/修改的 `.rs` 统计：最大 826 行（`src/server_test.rs`），无文件超过 `STD-SIZE-001` 的 1000 行上限。 |
| 功能目录 | `python3 scripts/check-feature-catalog.py` | PASS（本记录与 F-EXT-016 映射落盘后）。 |
| 用户配置 | `sha256 .mcp.json` | 未被本次改造修改：mtime `2026-11-23 14:40:20`（早于全部工作流启动），sha256 `8f5f5d119d1fcfbd3e7036516402bb92ad172737fb866c9dac9d354b010cedc2`。内容仍是用户此前的 stdio 编辑。 |

## 契约测试覆盖

`tests/mcp_http.rs` 用真实 TCP listener 与 Peri 等价 Auto 客户端（`server/discover` 拒绝 → legacy `initialize` → `Mcp-Session-Id`）覆盖：

- 协商、绑定与权限：非法/失败 initialize 不分配 CDP 或发布 Live；无 ID、未知 ID、无标记、伪造 `_meta` 均被拒；schema 查询不建会话；Host/Origin 与未知 monitor token。
- 会话隔离：共享引擎的两个会话互不可见页面、活动页与快照；不同引擎可返回相同 targetId 而公共 pageId 不混叠；两个并发 bind 仅一个成功。
- 单 writer 与 handoff：在途操作时接管显示 pending；接受/拒绝/超时/取消相关；idle 让出与人工 lease；外部 DevTools 409 不被误分类；未确认 Close 时拒绝替换 writer。
- 关闭与重连：SSE resume 不重复执行写；DELETE 先撤准入再等待 writer/viewer/WS；shutdown 退役旧 ID/token 且不关闭 target；attached POST 404 后 SDK 重试不重放旧写。
- Onboarding 与进程：无引擎时可 `browser_status` 并给出 install/serve/connect；错误文案与资源字节；SIGTERM 回收 listener 且日志不含 HTTP/CDP 原始输入。

## 真实引擎冒烟（Kitesurf 0.5.2，2026-11-23）

在同一台机器启动 `kitesurf serve --port 18972` 与网关 `--bind 127.0.0.1:18791 --cdp-url http://127.0.0.1:18972`，用 curl 以 Peri 等价 legacy 会话走通：

| 步骤 | 观察结果 |
| --- | --- |
| `initialize` / `notifications/initialized` | PASS，session id 正常分配。 |
| `browser_connect` | PASS：`connected=true`，`endpoint=ws://127.0.0.1:18972/devtools/browser`，`Kitesurf/0.5.2`。 |
| `new_page about:blank` | 初次 FAIL：409 `another_controller`。根因是页面 controller 的 `owner` 用了页 ID（`p_...`），而 Kitesurf 要求它与 browser controller 的 `owner` 一致；改为配置 owner 后 PASS，`hasController=true`。 |
| `take_snapshot` / `list_pages` | PASS。 |
| `DELETE`（无页面） | 202。 |
| `DELETE`（有页面） | 修复前 **500**：`local resources closed, but the engine did not confirm release`，0.6ms 返回。诊断显示 writer 连接关闭后收到 `Protocol(ResetWithoutClosingHandshake)`：Kitesurf 0.5.2 收到 Close 帧后直接关闭 TCP，不回复 WebSocket Close。修复后 PASS，202，`关闭 CDP 连接 confirmed=true`。 |

独立 CDP 探针确认 owner 语义：browser controller 以 `owner=peri-browser` 建 target 后，page WS 只有 `?owner=peri-browser` 能连上，`?owner=probe` 与 `?owner=p_test` 均被拒绝。修复后有回归用例断言 controller owner 跟随配置而非页 ID。关闭语义的修复在 `249fe5ae6`：对端 EOF/`ResetWithoutClosingHandshake` 与 `ConnectionClosed` 一样视为连接已释放，并新增 fixture `reset_on_close` 模式下带页面 DELETE 的回归用例。

## 单一二进制 CLI、安装脚本与发布通道（2026-11-23）

`peri-browser` 现在由 `src/cli.rs` 分派 `serve`（裸调用同义）、`install kitesurf`、`start kitesurf`，由 `src/lifecycle.rs` 下载安装器并在前台网关启动前拉起引擎；对外安装入口是 `scripts/install-browser.sh`，发布通道是 `.github/workflows/release-browser.yml`（tag `browser-v*`，仅 macos-aarch64 与 linux-x86_64）。本节证据采集于 `eae68a04d` 的工作区，改动与本记录同批提交；由执行代理在隔离 `HOME`/`PATH` 下运行，协调者复核了报告与关键行为，仍不是独立复验。2026-11-24 起 `start kitesurf` 改为后台启动并新增 `stop`，见下节。

| 检查 | 观察结果 |
| --- | --- |
| 单元与契约 | lib 137（含 cli 8、lifecycle 4、help 文案 1）、`tests/mcp_http` 19、doc 0；clippy `-D warnings` 与 fmt PASS。 |
| `install kitesurf` 已安装 | fixture 命中已有安装，exit 0，不触网、不执行安装器。 |
| `install kitesurf` 未安装 | exit 1，错误为 `kitesurf is not installed; run 'peri-browser install kitesurf' first`；无 `HOME` 时 exit 1 并提示设置 `KITESURF_INSTALL_DIR`。 |
| 安装器临时目录 | 每次运行创建 0700 私有目录并以 `create_new` 预留脚本文件，退出后清理；单测覆盖目录唯一、权限与重复清理。 |
| `start kitesurf` 复用 | 假引擎 `/json/version` 于 18973 可达时直接复用，打印 mcp/monitor，网关 `/healthz` 200，SIGTERM 后 exit 0。 |
| `start kitesurf` 等待 | 引擎不监听时 15s 后 exit 1，子进程回收，错误指向 `$HOME/.kitesurf/serve-<port>.log`；fixture 以 7 退出时立即失败并带退出码。 |
| 网关接线 | MCP `initialize` 的 `serverInfo.version` 取 `version()`；`browser_status` 的 recommendedEndpoint 跟随 `start kitesurf --port`。 |
| `install-browser.sh` | 离线 harness 37 项通过：平台映射与拒绝不支持平台、按 `browser-v` 前缀选最新 release、SHA-256 篡改或缺失即失败、`curl \| bash`、同版本重装与旧版本清理、跳过引擎开关。未联网下载、未真实安装。 |
| `release-browser.yml` | YAML 解析、9 个 `run` 块 `bash -n`、触发/矩阵/版本注入/资产名/`contents: write` 断言通过；未在 GitHub 上执行。 |

## 后台 `start` / `stop`（2026-11-24）

基线与范围：分支 `feat/browser-daemon`，`4860d84a5`（自 `origin/main`）上的工作区改动，未提交。`serve` 保持前台阻塞与 SIGTERM 行为；新增 `start [OPTIONS]`（后台网关）、`start kitesurf`（引擎就绪后后台网关，替换原先的前台阻塞）与 `stop [--bind IP:PORT]`。实现见 `src/daemon.rs`，CLI 分派见 `src/cli.rs`，引擎复用/拉起仍由 `src/lifecycle.rs` 的原有逻辑承担。状态与日志位于 `$HOME/.peri/browser/run|logs`（0700），状态文件 `run/gateway-<port>.json` 记录 `pid`、`pid_start`、`bind`、`log`、`engine_port`、`engine_owned`、`started_at`。

环境：macOS 26.6.2 arm64，Rust 1.99.0；exports 与本记录上方相同（`MISE_PYTHON_VERSION=3.12.15`、`CARGO_HOME=/tmp/cargo-home-rsproxy`、`RUSTUP_TOOLCHAIN=1.99.0`、`CARGO_INCREMENTAL=0`）。

| 检查 | 命令 | 观察结果 |
| --- | --- | --- |
| 构建 | `./scripts/cargo-rmcp-patched.sh build -p peri-browser-gateway --offline --locked` | PASS，`Finished dev profile`。 |
| 测试 | `./scripts/cargo-rmcp-patched.sh test -p peri-browser-gateway --offline --locked` | PASS：lib 153（含 daemon 14、cli 2）、`tests/mcp_http` 21（含 `daemon` 契约 2，其余 19 保留含 `process` 的前台 `serve` SIGTERM 断言）、doc 0。 |
| Clippy | `./scripts/cargo-rmcp-patched.sh clippy -p peri-browser-gateway --all-targets --offline --locked -- -D warnings` | PASS。 |
| 格式 | `./scripts/cargo-rmcp-patched.sh fmt -p peri-browser-gateway -- --check` | PASS。 |
| 功能目录 | `python3 scripts/test-feature-catalog.py && python3 scripts/check-feature-catalog.py` | PASS；F-EXT-016 行 visible 91 字符（≤100）。 |

契约测试 `tests/contracts/daemon.rs` 使用 `env!("CARGO_BIN_EXE_peri-browser")`、隔离 `HOME` 与 `TcpListener::bind("127.0.0.1:0")` 取得的临时端口：

| 用例 | 观察结果 |
| --- | --- |
| `start_returns_after_the_health_check_and_stop_is_idempotent` | `start --bind 127.0.0.1:<port>` 在 15s 内 exit 0；状态文件字段齐全（`engine_port=null`、`engine_owned=false`、`started_at>0`）；`/healthz` 200 且 `ok=true`；同 bind 第二次 `start` exit 1 并含 pid 与 `peri-browser stop`，状态保留；`stop` 5s 内 exit 0、状态删除、端口 5s 内不再接受连接；再次 `stop` exit 0 且含 `not running`。 |
| `start_kitesurf_does_not_own_a_reused_engine` | 用进程内 fixture 引擎（`/json/version` 可达，非真实 kitesurf）：`start kitesurf --port <fixture> --bind <port>` exit 0，打印复用提示与 mcp 地址；状态 `engine_port=<fixture>`、`engine_owned=false`；`stop` exit 0 且 fixture 引擎仍响应 `/json/version`。未执行 `kitesurf` 二进制。 |

手工冒烟（真实二进制 `target/debug/peri-browser`，隔离 `HOME=/tmp/peri-daemon-*`，端口 18794/18796/18797）：

| 步骤 | 观察结果 |
| --- | --- |
| `start --bind 127.0.0.1:18794` | exit 0；输出 mcp/monitor/pid/log/stop 五行；`run/`、`logs/` 为 0700；状态文件与日志生成；`GET /healthz` 200。 |
| 同 bind 再次 `start` | exit 1，stderr 含 `pid <pid>` 与 `peri-browser stop --bind 127.0.0.1:18794`；状态保留。 |
| 陈旧状态（pid 999999）后 `start` | 打印 `removed stale gateway state ...` 后正常拉起，exit 0。 |
| `stop --bind [::1]:18794` | 打印 `gateway on [::1]:18794 is not running`，exit 0；IPv4 网关仍返回 200（状态按端口分文件，`stop` 不跨 loopback 地址）。 |
| `stop --bind 127.0.0.1:18794` | exit 0，端口关闭，状态删除；再次 `stop` exit 0 且 `not running`。 |
| `start --bind 127.0.0.1:0` | exit 1，提示 `start` 需要固定端口。 |
| `start`/`stop --bind [::1]:18797` | 均 exit 0；状态 `bind=[::1]:18797`；`GET http://[::1]:18797/healthz` 200；stop 后端口关闭、状态删除。 |
| 状态指向存活非网关进程（`/bin/sleep`） | `stop` exit 1，stderr 为 `refusing to stop pid <pid> ... process is /bin/sleep, not peri-browser`；状态保留且 sleep 仍存活；sleep 退出后再次 `stop` exit 0 并清理状态（macOS `ps -p <pid> -o comm=` 路径）。 |
| 子进程提前退出（外部先占住 18798 后 `start`） | exit 1，stderr 为 `gateway exited before becoming healthy (exit status: 1); see .../gateway-18798.log`；`run/` 为空；日志含 `Address already in use (os error 48)`。 |

冒烟结束后确认无遗留 `peri-browser` 后台进程；18791 上的既有网关未触碰。未运行项见文末。

### CodeRabbit 修正轮（2026-11-20）

| 检查 | 命令 | 观察结果 |
| --- | --- | --- |
| 构建 | `./scripts/cargo-rmcp-patched.sh build --offline --locked -p peri-browser-gateway` | PASS。 |
| 测试 | `./scripts/cargo-rmcp-patched.sh test --offline --locked -p peri-browser-gateway` | PASS：lib 153、`tests/mcp_http` 21；daemon 单测由 10 增至 14。 |
| Clippy | `./scripts/cargo-rmcp-patched.sh clippy --offline --locked -p peri-browser-gateway --all-targets -- -D warnings` | PASS。 |
| 格式 | `./scripts/cargo-rmcp-patched.sh fmt -p peri-browser-gateway -- --check` | PASS。 |
| 功能目录 | `python3 scripts/test-feature-catalog.py && python3 scripts/check-feature-catalog.py` | PASS（22 tests）。 |

本轮收紧的行为：

- **状态发布时机与排他性**：`start` 不在 spawn 后写状态，只在 `/healthz` 返回子进程自身 pid 后用同目录临时文件 + `hard_link` 发布；并发失败方终止自己的子进程并保留赢家状态，陈旧赢家删除后只重试一次。`start` 在 spawn 前探测绑定地址，已有未记录的 `/healthz` 应答时拒绝接管。
- **健康校验**：`/healthz` 增加 `pid` 字段；就绪判定要求该 pid 等于本次子进程 pid，否则继续轮询并保留 `try_wait`。健康请求读取完整响应 body，不再只看响应头。
- **`stop` 可重试**：先停网关，再计算 `engine_stop_target` 并执行 `kitesurf stop`，成功后才删除状态；引擎失败保留状态并返回 1。`NotRunning`（pid 已死或启动标识不同）同样先尝试引擎清理。
- **进程身份**：状态新增可选 `pid_start`（Linux `/proc/<pid>/stat` 第 22 字段，macOS `ps -p <pid> -o lstart=`）；记录值与当前值都存在时必须相等，缺失时回退 `comm` basename。pid 复用不再触发 SIGTERM。
- **新增单测**：`publishing_state_is_exclusive_and_never_replaces_the_winner`、`a_reused_pid_with_a_different_start_identity_is_not_the_gateway`、`missing_start_identity_falls_back_to_the_comm_basename`、`a_missing_pid_start_is_omitted_and_old_states_still_parse`；契约测试新增 `/healthz` pid 与状态 `pid_start` 断言，并验证第二次 `start` 不改写赢家状态。

### update / update kitesurf（2026-11-24）

范围：新增 `peri-browser update`，不接受参数，只复用官方 `scripts/install-browser.sh`，不复制检查版本、下载、校验与软链切换逻辑。实现为 `src/cli.rs` 的 `Command::Update` 与 `src/main.rs` 的 `update()`：以 `bash -c 'set -o pipefail; curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-browser.sh | bash'` 执行，stdin/stdout/stderr 与环境变量继承，退出码原样返回；不停止正在运行的网关。基线 `cebfddcab`（`origin/main`），证据由执行代理采集，不是独立复验。

同一轮补充 `update kitesurf`：`src/cli.rs` 的 `parse_update` 接受 `[]` 与 `["kitesurf"]`，分别得到 `Command::Update` 与 `Command::UpdateKitesurf`；后者分派到 `src/lifecycle.rs` 的 `update_kitesurf`。`install` 与 `update_kitesurf` 共用 `install_or_update(lookup, force)`，`install` 保持 `force = false`，`update_kitesurf` 用 `force = true` 跳过“已安装即返回”短路，先打印 `updating kitesurf <version> ...`（无版本时 `updating kitesurf ...`，未安装时 `kitesurf is not installed; installing the latest release`），再原样返回 kitesurf 官方安装器的退出码。`install kitesurf` 的幂等行为未变。

环境：macOS 26.6.2 arm64，Rust 1.99.0；exports 与本记录上方相同。

| 检查 | 命令 | 观察结果 |
| --- | --- | --- |
| 构建 | `./scripts/cargo-rmcp-patched.sh build --offline --locked -p peri-browser-gateway` | PASS，`Finished dev profile`。 |
| 测试 | `./scripts/cargo-rmcp-patched.sh test --offline --locked -p peri-browser-gateway` | PASS：lib 157（新增 `update_takes_an_optional_kitesurf_target`、`unknown_update_targets_point_at_the_supported_target_and_the_gateway`、`help_separates_bare_update_from_update_kitesurf` 与 lifecycle 的 `update_without_a_home_or_override_fails_before_any_download`）、`tests/mcp_http` 21、doc 0。 |
| Clippy | `./scripts/cargo-rmcp-patched.sh clippy --offline --locked -p peri-browser-gateway --all-targets -- -D warnings` | PASS。 |
| 格式 | `./scripts/cargo-rmcp-patched.sh fmt -p peri-browser-gateway -- --check` | PASS。 |
| 功能目录 | `python3 scripts/test-feature-catalog.py && python3 scripts/check-feature-catalog.py` | PASS（22 tests）。 |
| 多余参数 | `target/debug/peri-browser update kitesurf extra` | exit 1，stderr `update accepts at most one target; unexpected "extra"; see --help`。 |
| 未知目标 | `target/debug/peri-browser update foo` | exit 1，stderr `unknown update target "foo"; supported: kitesurf; run peri-browser update without a target to update the gateway`。 |
| 帮助 | `target/debug/peri-browser update --help` | 全局 help，列表含 `peri-browser update [kitesurf]`，说明裸 `update` 复用 browser 安装脚本且已安装的引擎不会升级、`update kitesurf` 无条件重跑 kitesurf 官方安装脚本，并提示安装器不停止运行中的进程、需 `stop` 后再 `start`。 |
| `bash` 缺失 | `env PATH=/nonexistent target/debug/peri-browser update` | exit 1，stderr `failed to run bash ... update needs bash and curl on PATH`。 |
| `curl` 缺失 | PATH 只含 `bash` 时执行 `update` | exit 127，`bash: curl: command not found`；`pipefail` 使空脚本未误报成功。 |
| 真实安装（隔离目录） | `PERI_BROWSER_INSTALL_DIR=<tmp> PERI_BROWSER_SKIP_KITESURF=1 PERI_BROWSER_NO_PATH_HINT=1 target/debug/peri-browser update` | **失败**：`curl: (35) Recv failure: Connection reset by peer`，`update` 原样返回 35，临时目录为空；同环境直接执行同一条 `curl \| bash` 也返回 35，判定为网络出口问题而非实现差异。两次重试仍失败，成功安装未验证。 |
| 失败传播 | 上述失败运行 | stderr 含 `peri-browser: update failed (exit code 35); check network access and that curl and bash are on PATH`；非零退出未被吞掉。 |
| 短路绕过 | 同环境（PATH 上有 mise 安装的 kitesurf 0.5.2）依次执行 `install kitesurf` 与 `update kitesurf` | `install` 打印 `kitesurf already installed: …/0.5.2/kitesurf` 并 exit 0；`update kitesurf` 打印 `updating kitesurf ...` 后进入下载，exit 35。已安装短路被绕过。 |
| 状态行（有版本） | `env -i HOME=<tmp> PATH=/usr/bin:/bin:/usr/sbin:/sbin KITESURF_INSTALL_DIR=<tmp>/kitesurf KITESURF_NO_PATH_HINT=1 target/debug/peri-browser update kitesurf`，隔离目录内含 `bin/kitesurf` 与 `current-version.txt=9.9.9` | stdout `updating kitesurf 9.9.9 ...`，随后 `curl: (35) Recv failure: Connection reset by peer`，exit 35。 |
| 状态行（未安装） | 同上但隔离目录为空、HOME 与 PATH 均无 kitesurf | stdout `kitesurf is not installed; installing the latest release`，随后同样 exit 35。 |
| 端到端（隔离目录） | `KITESURF_INSTALL_DIR=<tmp>/kitesurf KITESURF_NO_PATH_HINT=1 target/debug/peri-browser update kitesurf` | **失败**：stdout `updating kitesurf ...`，stderr `curl: (35) Recv failure: Connection reset by peer`，exit 35，`<tmp>/kitesurf` 未创建。另两次重试结果相同，均为网络出口重置，成功安装未验证。 |

未验证项：隔离目录中的 `current-version.txt` 与 `bin/peri-browser --version` 因网络重置未取得；`update kitesurf` 的真实成功安装及其 `current-version.txt` / `bin/kitesurf --version` 同样未取得；更新后 `stop` 再 `start` 切换到新二进制（网关或引擎）未运行（未触碰 18791 的既有网关与本机 0.5.2 引擎）；Windows 仍走 `#[cfg(not(unix))]` 的 “Unix platform required”；安装脚本 SHA-256 校验失败路径由既有离线 harness 覆盖，本次未重复。

## 未运行与剩余风险

- **真实引擎仅覆盖最小路径**：已用 Kitesurf 0.5.2 走通 initialize/connect/new_page/snapshot/list，以及带页面会话的 DELETE；未验证 monitor 浏览器交互、yield/request_human、真实导航与 fill、0.5.4、多会话并发。
- **独立复验未完成**：原计划的 verification agent 三次均被 provider 错误终止；上表由协调者执行。
- **外部 Peri 进程经 HTTP 消费未联测**：`/mcp` 由真实 listener 与 SDK 客户端验证，但未用完整的 Peri 宿主配置跑通。
- **跨平台与发布物未验证**：Linux、Windows、macOS x64、发布包在真实平台的运行、`rg` 依赖的仓库扫描均未运行。
- **发布与安装入口未端到端验证**：`release-browser.yml` 未在 GitHub Actions 上执行，`install-browser.sh` 未对真实 Release 下载与安装，`browser-v*` / `agent-v*` tag 未推送；PR CI、CodeRabbit 与合并结果也不在本记录证据内。
- **后台网关未接真实引擎**：`start`/`stop` 用真实二进制与隔离 `HOME` 验证；`start kitesurf` 只走 fixture 复用路径，真实引擎的拉起后后台化、`kitesurf stop --port N`、Linux `/proc/<pid>/stat` 启动标识分支、多网关共享引擎的端到端停止（单测覆盖判定）均未运行；`start kitesurf` 的引擎子进程跨命令退出继续运行只由代码与 fixture 推断，未对真实引擎观测。引擎清理失败后的保留状态/重试路径未用真实 `kitesurf` 构造；健康检查 10s 超时后的 SIGTERM 分支未构造到（子进程提前退出与 `stop` 的 SIGTERM 路径已实测）。
- **残余风险**：monitor token 出现在 URL；网关无鉴权，安全边界是本机回环与 Host/Origin 校验；fixture fidelity 与真实引擎仍可能有差异；`scripts/install.sh` 用 `releases?per_page=30` 查找最新 agent release，browser tag 增多后可能把 agent tag 挤出窗口。
- 改造按逻辑分步提交（本分支提交与本记录）；记录时未推送。
