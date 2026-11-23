# peri-browser-gateway MCP 合并验收记录

> 基线：`local/peri-browser-check`，HEAD `1d97f3f223328c20916b633c8c081cd20d9a127d` 加本次未提交改造；2026-11-23。平台 macOS 26.6.2 arm64，Rust 1.99.0，`CARGO_TARGET_DIR=/tmp/peri-browser-target`，patched rmcp 3.9.0、tokio 1.53.1、axum 0.8.9。功能行见 [F-EXT-016](../reference/feature-catalog.md)，运行时契约见 [gateway README](../../mcp-packages/peri-browser-gateway/README.md)。

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

| 检查 | 命令 | 观察结果 |
| --- | --- | --- |
| 构建 | `./scripts/cargo-rmcp-patched.sh build --offline --locked -p peri-browser-gateway --bin peri-browser-gateway` | PASS，`Finished dev profile`。 |
| 测试 | `./scripts/cargo-rmcp-patched.sh test --offline --locked -p peri-browser-gateway` | PASS：lib 123，`src/main.rs` 0，`tests/mcp_http` 18，doc 0。18 项由 3 个 transport 级用例与 15 个契约模块用例组成（`monitor_handoff` 7、`shutdown` 4、`session_isolation` 2、`onboarding_wire` 1、`process` 1）。 |
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

## 未运行与剩余风险

- **真实 Kitesurf 引擎 smoke 未运行**：未验证 0.5.4 目标二进制、真实 409/Close 时序、真实导航/fill 与 monitor 浏览器交互。fixture 的两个 writer 409、共享 targetId 与可闸住 upgrade 只模拟边界，不替代真实引擎。
- **独立复验未完成**：原计划的 verification agent 三次均被 provider 错误终止；上表由协调者执行。
- **外部 Peri 进程经 HTTP 消费未联测**：`/mcp` 由真实 listener 与 SDK 客户端验证，但未用完整的 Peri 宿主配置跑通。
- **跨平台与发布物未验证**：Linux、Windows、macOS x64、发布包安装路径、`rg` 依赖的仓库扫描均未运行。
- **残余风险**：monitor token 出现在 URL；网关无鉴权，安全边界是本机回环与 Host/Origin 校验；fixture fidelity 与真实引擎仍可能有差异。
- 未提交、未推送；本记录落盘后再按逻辑分步提交。
