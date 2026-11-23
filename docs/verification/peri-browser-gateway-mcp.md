# peri-browser-gateway MCP 合并验收记录

> 基线：`local/peri-browser-check`，`1d97f3f223` 加本分支四个提交（`42ef2e669` 合并、`860df7e5e` 文档、`bc04e8064` 页面 owner 修复、`249fe5ae6` 引擎断开视为已释放）；2026-11-23。平台 macOS 26.6.2 arm64，Rust 1.99.0，patched rmcp 3.9.0、tokio 1.53.1、axum 0.8.9。功能行见 [F-EXT-016](../reference/feature-catalog.md)，运行时契约见 [gateway README](../../mcp-packages/peri-browser-gateway/README.md)。

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

## 未运行与剩余风险

- **真实引擎仅覆盖最小路径**：已用 Kitesurf 0.5.2 走通 initialize/connect/new_page/snapshot/list，以及带页面会话的 DELETE；未验证 monitor 浏览器交互、yield/request_human、真实导航与 fill、0.5.4、多会话并发。
- **独立复验未完成**：原计划的 verification agent 三次均被 provider 错误终止；上表由协调者执行。
- **外部 Peri 进程经 HTTP 消费未联测**：`/mcp` 由真实 listener 与 SDK 客户端验证，但未用完整的 Peri 宿主配置跑通。
- **跨平台与发布物未验证**：Linux、Windows、macOS x64、发布包安装路径、`rg` 依赖的仓库扫描均未运行。
- **残余风险**：monitor token 出现在 URL；网关无鉴权，安全边界是本机回环与 Host/Origin 校验；fixture fidelity 与真实引擎仍可能有差异。
- 改造按逻辑分步提交（见基线的四个提交与本记录）；未推送。
