# Peri Swarm 副本组与外部 engine 配置验收（M1）

- 状态：代码与自动化证据记录；**M0 未执行**，人工双进程矩阵未执行
- 日期：2026-11-21
- 基线：`c5bb30155`（M1 代码起点，共享工作区，M1 改动未提交）。执行期间协调者另行
  提交了 `32642434f`（只改 `peri-swarm/README.md`），不影响本记录的代码与文档变更。
- 范围：计划 M1 的代码增量——Rivet endpoint/token/namespace/pool 环境配置、
  `EngineSpawnMode` 可配置、`PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 开关、配置矛盾与
  外部 engine 不可达的启动期显式失败——以及同变更的文档。
- 明确不在本记录内：双进程多副本行为、单 engine 节点容量、TLS、engine 多节点、
  1000 Agent。本文只写实际执行过的证据。

## 1. 组件与版本

| 组件 | 版本/标识 |
| --- | --- |
| 主机 | macOS 26.6.2（arm64） |
| Rust | rustc 1.99.0（`b940084d7` 2026-09-28）；cargo 1.102.0 |
| rivetkit | 3.1.1（`Cargo.lock`） |
| turso_serverless | 0.1.3（仓库补丁缓存 `target/peri-turso-patches/…`） |
| Rivet engine | 未启动（本记录不含 engine 运行证据） |
| Loom | 未连接 |
| 依赖源 | crates.io 在本机不可解析（`curl` 报 `Could not resolve host: static.crates.io`）。命令通过命令行加 `--config source.crates-io.replace-with="rsproxy"` 与 `--config source.rsproxy.registry="sparse+https://rsproxy.cn/index/"` 使用镜像；仓库与用户 cargo 配置均未修改。 |

## 2. 实际执行的命令与结果

除非另行注明，命令都带上述两个 `--config` 参数（镜像仅替换下载源，`Cargo.lock`
校验和与 `--locked` 不变）；check/test 时它们放在子命令之前，clippy 见第 5 行。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `./scripts/cargo-rmcp-patched.sh check --locked -p peri-swarm --all-targets` | 通过（`Finished dev profile`，exit 0） |
| 2 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` | 通过：lib 174、`actor_runtime_test` 7、`http_contract_test` 20、`items_pagination_test` 6、`mcp_injection_test` 4、`openai_hosted_test` 7、`streaming_test` 11；ignored 6（`actor_runtime_test` 1、`loom_store_test` 3、`rivet_actor_test` 2）；doc-tests 0。0 failed |
| 3 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib bootstrap::tests` | 通过：43 passed，0 failed |
| 4 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib config::tests` | 通过：11 passed，0 failed |
| 5 | `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets --config 'source.crates-io.replace-with="rsproxy"' --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"' -- -D warnings` | 通过（`Finished dev profile`，exit 0，无 warning；格式化前后各跑一次，46.50s / 1m28s）。两个 `--config` 放在 `clippy` 子命令之后：cargo-clippy 会把子命令后的参数转给内部的 `cargo check`，全局位置的 `--config` 不会传下去（首次尝试因此失败在拉取 index）。 |
| 6 | `python3 scripts/check-feature-catalog.py` | 通过：`Documentation: PASS (structure and destinations; not runtime acceptance)` |
| 7 | `PATH=/Users/mino/.cargo/bin:$PATH CARGO_HOME=/tmp/peri-cargo-home bash scripts/check-changed.sh` | 通过：`cargo check --locked --workspace`，`Finished dev profile`，exit 0（2m24s / 热缓存复跑 1m46s）。`changed-crate-args.sh` 在本机离线环境无法执行 `cargo metadata --no-deps`（stderr：`[changed-crate-args] cargo metadata 不可用，回退 --workspace`），按脚本设计回退为全工作区检查（更严方向）。临时 `CARGO_HOME` 只含镜像 source 替换与指向既有 `registry`/`targets` 的符号链接，仓库与用户配置未改。 |
| 8 | `git diff --check` | 通过（exit 0，无输出） |

覆盖的行为（测试名称见 `peri-swarm/src/bootstrap_test.rs` 与
`peri-swarm/src/config_test.rs`）：

- 未设置任何新变量时，endpoint/namespace/pool/spawn/token 与 `c5bb30155` 的常量
  一致（endpoint 去掉 `Url` 尾部 `/` 后逐字节相同）；
- endpoint 形状（`http` only、显式端口、无 userinfo/query/fragment、path 为空或 `/`）、
  `https` 显式拒绝；
- 非 loopback endpoint 必须 `never` 且必须提供 token 变量名；`always`/`auto` 与非
  loopback 组合、缺 token 变量时错误变量名精确断言；
- token 只存变量名：值形似 token、与 `PERI_SWARM_STORE_TOKEN_ENV` 同名、被命名变量
  缺失或为空都显式失败；`BootstrapConfig`/`RivetConfig` 的 `Debug` 不含 sentinel 值；
- namespace/pool 的空、空白、超长与自定义值；
- bind 开关：`1/true/yes/on` 放行、`0/false/no/off` 保持 loopback、其他值显式失败，
  未开开关时错误消息同时包含 `loopback` 与开关变量名；
- `never` 的 5s TCP 预检：不可达返回 `RivetUnreachable`（消息含 endpoint、说明不会
  拉起本地 engine），可达监听端口通过；`always`/`auto` 不做预检。

## 3. 未执行项与原因

1. **M0 未执行。** M0 需要在当前代码上启动两个进程并探测共享 engine、双 Loom 并发
   打开等行为，依赖活体 Loom 与 Rivet engine；本次环境只具备编译依赖，未启动任何
   外部组件。本记录因此不含任何 M0 探针结论，也不推断「B 能否复用同一 engine」。
2. **人工双进程矩阵（计划 §7.3）未执行。** 同上：需要真实 Loom 双库、Rivet 3.1.1
   engine、共享 workspace 根与 stub 模型。矩阵的 10 个判据均无运行证据。
3. **新增的 `#[ignore]` 双 registry 测试未执行。**
   `an_attaching_registry_recovers_actors_started_by_the_spawning_registry` 需要本机
   engine（`RIVETKIT_ENGINE_AUTO_DOWNLOAD=1`）；本次只验证它能编译。运行命令：
   `cargo-rmcp-patched.sh test --locked -p peri-swarm --test rivet_actor_test --
   --ignored --exact an_attaching_registry_recovers_actors_started_by_the_spawning_registry`。
4. **engine 多节点、engine TLS、engine 重启重连、1000 Agent 容量**：未执行，也不在
   M1 的代码范围内。

## 4. 证据边界

- 单元测试只证明配置解析、错误变量名与失败路径；不证明真实 engine 路由、多进程
  Loom 并发或副本恢复。
- `install_deployment` 是**进程级**：新测试即使运行，两个 registry 也只共享本进程的
  store 与 actor 配置，等价「共享 Loom」，不等价「两台主机」，也不能替代多节点 engine
  或主机级故障验证。
- 5s 预检只证明 TCP 可达；engine 握手、鉴权与健康仍由 registry 任务报告。
- 配置面只支持私网明文 `http` endpoint；`https` 被显式拒绝。

## 5. 端点私网校验（核验 FAIL 修复）

- 日期：2026-11-21
- 基线：`05d93fe1e` 工作树；范围仅限 Rivet endpoint 校验、对应单测与上述文档行。
- 原第 1–4 节保持原样；本节记录独立核验结论、修复与复验证据。

### 5.1 核验发现

1. [高] `validate_rivet_endpoint` 接受 `http://192.0.2.1:6420` 这类公网字面 IP，
   与设计、README、验收记录声称的「仅私网明文」不一致；engine token 可能被明文
   发往公网。
2. [低] `http://host:80` 被误判为缺少显式端口：`url::Url::port()` 对 scheme 默认
   端口返回 `None`。
3. [低] 设计文档称绑定开关允许绑定「私网地址」，代码实际接受 `0.0.0.0`。

### 5.2 修复

- 单一判定位置仍是 `peri-swarm/src/config.rs::validate_rivet_endpoint`；新增
  `ensure_private_engine_host` 与 `has_explicit_port` 两个私有 helper，不引入第二套
  地址策略。
- 字面 IPv4 只接受 loopback（`127.0.0.0/8`）、RFC1918（`10/8`、`172.16/12`、
  `192.168/16`）与 link-local（`169.254/16`）；字面 IPv6 只接受 `::1`、ULA
  （`fc00::/7`）与 link-local（`fe80::/10`）。公网、`0.0.0.0`、`::`、组播与广播
  在解析阶段失败，错误消息包含 endpoint 与具体拒绝原因。
- 域名不做 DNS 解析，直接接受；设计文档已写明由部署保证指向私网/环回，代码不解析。
- 端口校验改为检查原始 authority，而不是 `Url::port()`：`http://host:80` 与
  `http://[::1]:80` 按显式端口接受，`http://host`、`http://[::1]` 仍拒绝。
- 绑定行为不变（继续接受 `0.0.0.0`，容器需要），只把设计文档措辞改为「非 loopback
  绑定（含 `0.0.0.0`）需显式开关并输出 WARN，TLS 必须由入口终止」。
- 文档同步：设计 §5 Rivet 配置段与 §8 第 5 条、功能目录与验收映射的 F-SWARM-011 行。

### 5.3 字面地址矩阵

| 输入 | 结果 | 原因/说明 |
| --- | --- | --- |
| `http://127.0.0.1:6420`、`http://127.255.255.255:6420` | 接受 | loopback |
| `http://10.1.2.3:6420`、`http://172.16.0.1:6420`、`http://192.168.1.1:6420` | 接受 | RFC1918 |
| `http://169.254.1.1:6420` | 接受 | IPv4 link-local |
| `http://[::1]:6420` | 接受 | IPv6 loopback |
| `http://[fc00::1]:6420`、`http://[fd00::1]:6420` | 接受 | IPv6 ULA |
| `http://[fe80::1]:6420` | 接受 | IPv6 link-local |
| `http://rivet.internal:6420` | 接受 | 域名不解析，部署保证指向私网/环回 |
| `http://host:80`、`http://[::1]:80` | 接受 | 写出的默认端口也算显式端口 |
| `http://192.0.2.1:6420`、`http://8.8.8.8:6420` | 拒绝 | `a public address` |
| `http://172.32.0.1:6420`、`http://192.169.0.1:6420`、`http://169.255.0.1:6420` | 拒绝 | RFC1918/link-local 边界外，`a public address` |
| `http://0.0.0.0:6420` | 拒绝 | `the unspecified address` |
| `http://224.0.0.1:6420` | 拒绝 | `a multicast address` |
| `http://255.255.255.255:6420` | 拒绝 | `the broadcast address` |
| `http://[::]:6420` | 拒绝 | `the unspecified address` |
| `http://[2001:db8::1]:6420`、`http://[fec0::1]:6420` | 拒绝 | `a public address` |
| `http://[ff02::1]:6420` | 拒绝 | `a multicast address` |
| `https://127.0.0.1:6420` | 拒绝 | 明文 HTTP 之外显式拒绝，提示 engine TLS 需另行设计 |
| `http://host`、`http://[::1]`、`http://host/path` | 拒绝 | `explicit port is required` |

### 5.4 新增测试

`peri-swarm/src/config_test.rs`（直接覆盖单一判定位置）：

- `rivet_endpoint_accepts_loopback_private_ula_and_link_local_literals`
- `rivet_endpoint_accepts_domains_without_resolving_them`
- `rivet_endpoint_rejects_non_private_literal_addresses`
- `rivet_endpoint_accepts_a_written_default_port_and_still_requires_a_port`
- `rivet_endpoint_keeps_the_plaintext_and_shape_rules`

`peri-swarm/src/bootstrap_test.rs`（环境变量入口回归）：

- `a_public_or_unspecified_literal_endpoint_is_rejected`

### 5.5 复验命令与真实计数

环境：工作树 `/Users/mino/code/peri/.worktrees/swarm-rivet-endpoint`；
`CARGO_HOME=/tmp/cargo-home-rsproxy`（用户级源替换与本地 patched crate），
`PATH` 含 workspace 的 `cargo`/`python3`。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo fmt --check` | 通过（exit 0） |
| 2 | `cargo check -p peri-swarm --all-targets --locked` | 通过（`Finished dev profile`，exit 0） |
| 3 | `cargo test -p peri-swarm --locked` | 通过：lib 191、main 0、`actor_runtime_test` 7、`http_contract_test` 23、`items_pagination_test` 6、`mcp_injection_test` 4、`openai_hosted_test` 7、`streaming_test` 11；ignored 6（`actor_runtime_test` 1、`loom_store_test` 3、`rivet_actor_test` 2）；doc-tests 0。合计 249 passed、0 failed |
| 4 | `cargo test -p peri-swarm --locked --lib bootstrap::tests` | 通过：44 passed（修复前记录 43） |
| 5 | `cargo test -p peri-swarm --locked --lib config::tests` | 通过：16 passed（修复前记录 11） |
| 6 | `cargo clippy -p peri-swarm --all-targets --locked -- -D warnings` | 通过（`Finished dev profile`，exit 0，无 warning） |
| 7 | `python3 scripts/check-feature-catalog.py` | 通过：`Documentation: PASS (structure and destinations; not runtime acceptance)` |
| 8 | `git diff --check` | 通过（exit 0，无输出） |

### 5.6 证据边界

- 单测只证明解析期的地址与端口策略；不证明真实 engine 连接、网络隔离或访客可达性。
- 域名分支按契约不做 DNS 解析，因此无法在解析阶段证明其指向；若部署把域名解析到
  公网地址，token 仍会被发出。这是设计接受的残余风险，由部署负责。
- `Url` 会省略 scheme 默认端口，`http://host:80` 解析后序列化为 `http://host/`；
  连接语义仍是 80 端口，本次只保证「写出默认端口被接受」。
- 绑定行为未改：`0.0.0.0` 仍需显式开关并输出 WARN；TLS 终止仍由入口负责，未验收。
