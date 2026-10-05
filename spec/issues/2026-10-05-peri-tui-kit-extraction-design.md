# peri-tui `kit/` 拆分（独立 crate）可行性设计

状态：**设计草案，未实施**。本文只做可行性论证与边界设计，未改动任何源码，未运行 cargo。
上游依据：[Rust 编译性能诊断](./2026-10-05-rust-build-performance-diagnosis.md)（候选 C1）。
所有代码统计为 2026-10-05 在本 worktree（`perf/rust-build-speedup`）只读采集；编译耗时数据引自诊断
报告 §1.2/§1.3/§1.4，本机为 macOS / Apple Silicon 18 核 —— CI runner（2~4 核）参考性有限。

**结论先行**：`kit/` 拆成独立 crate **在依赖方向不变的前提下净收益接近零**（peri-tui 是依赖图的终端叶子，
拆链不缩链），且**不存在"先切一小块干净子集"的捷径**（严格闭合的宿主无关子集只有 3,833 行，占 kit 的 4.5%）。
值得做的形态是「共享底座下沉 + kit/host 并列」，此时预计回收终端段 1~2s（约 1~2%），
真正的量级收益（再 -3~8s）**必须以先行解耦 `kit → peri-middlewares` / `kit → peri-acp` 为前提**，
即与 C2/C3/M-TUI 任务线合流。建议 **不做纯粹的 C1，而把 C1 降级为 C2/C3 落地后的结构性收口**。

---

## 一、现状数据（代码核实）

### 1.1 kit 结构画像

**总量**（`peri-tui/src`，含测试）：

| 范围 | 文件数 | 行数 | 占比 |
| --- | --- | --- | --- |
| `peri-tui/src` 合计 | 333 | 102,711 | 100% |
| ├ `kit/` | 259 | **85,839** | **83.6%** |
| ├ `acp_client/` | 21 | 6,256 | 6.1% |
| ├ `components/` | 14 | 2,588 | 2.5% |
| ├ `app/` | 10 | 1,659 | 1.6% |
| ├ `config/` + `i18n/` + `thread/` | 6 | 634 | 0.6% |
| └ `src/` 根下散文件（`cli_*` / `launch` / `main` / `truncate` / `update` / `alloc_config` / `lib`） | 23 | 5,735 | 5.6% |
| `peri-tui/tests/`（集成测试，独立 target） | 4 | 2,171 | — |
| **peri-tui 全部 .rs** | 337 | **104,882** | 与报告 §1.3「10.3 万行」一致 |

**kit 生产/测试拆分**：生产 **51,715** 行，测试 **34,124** 行（`*_test*` 命名）+ 非 `_test` 文件内的
`#[cfg(test)]` 尾部块约 1,535 行 ⇒ **测试约占 kit 的 41%**。

**kit 分子目录**（`prod` 为不含 `*_test*` 命名文件的行数）：

| 子目录 | prod | test | 合计 |
| --- | ---: | ---: | ---: |
| `panels/` | 12,300 | 2,930 | 15,230 |
| `message_area/` | 7,824 | 6,707 | 14,531 |
| `acp_events_test/`（纯测试目录） | 0 | 7,412 | 7,412 |
| `markdown/` | 2,824 | 2,131 | 4,955 |
| `acp_events/` | 3,847 | 0 | 3,847 |
| `popups/` | 2,264 | 496 | 2,760 |
| `acp_types/` | 1,939 | 64 | 2,003 |
| `input_area/` | 1,124 | 593 | 1,717 |
| `setup_wizard/` | 1,230 | 63 | 1,293 |
| `tui_render_unit/` | 1,173 | 0 | 1,173 |
| `acp_notifier/` | 725 | 0 | 725 |
| `steer_queue/` | 360 | 237 | 597 |
| `service_snapshot/` | 92 | 0 | 92 |
| **`kit/` 下的散文件（93 个）** | **16,013** | **13,491** | **29,504** |
| 合计 | 51,715 | 34,124 | 85,839 |

**热点文件**（行数，含测试）：

| 文件 | 行数 | 说明 |
| --- | ---: | --- |
| `message_area/render_test.rs` | 3,876 | 测试 |
| `markdown/mod_test.rs` | 2,022 | 测试 |
| `acp_notifier_test.rs` | 1,429 | 测试 |
| `message_area/mod_test.rs` | 1,115 | 测试 |
| `tui_render_unit_test.rs` | 1,054 | 测试 |
| `acp_events/system.rs` | 975 | 生产 |
| `input_area.rs` | 946 | 生产 |
| `acp_events_test/session_events_test.rs` | 937 | 测试（报告 §归因 4 点名的 `acp_events_test.rs` 7412 行是**目录合计**，非单文件） |
| `acp_types_test.rs` | 922 | 测试 |
| `message_area/mod.rs` | 919 | 生产 |
| `atoms.rs` | 849 | 生产，**最大枢纽**（152 个 pub 项） |
| `service_snapshot.rs` | 804 | 生产 |
| `acp_bridge.rs` | 742 | 生产 |

**公共 API 面**：kit 内 `pub` 项 **702** 个、`pub(crate)` **212** 个，`kit/mod.rs` 顶层 `pub mod` **58** 个。
即：一旦成为 crate，85,839 行的内部结构会整体成为对外 API，`pub(crate)` 的 212 处需要逐项处理
（升级可见性、改为 crate 内 re-export，或经 `pub use` 转发）。

### 1.2 依赖方向

#### (a) `kit → peri-tui 其余部分`（**拆分的主要障碍**）

正则 `crate::(app|acp_client|components|config|i18n|launch|thread|truncate|update|alloc_config)` 统计：

| 目标模块 | 引用数（全部） | 非测试文件引用数 | 涉及文件数 | 关键符号 |
| --- | ---: | ---: | ---: | --- |
| `crate::i18n` | 170 | — | — | `init`(67)、`tr`(29)、`tr_args`(4)、`LcRegistry::new`(4)、`switch`(1) |
| `crate::acp_client` | 55 | — | — | `InteractionOwner`(20+)、`AcpTuiClient`(11+)、`InteractionUiOutcome`(6)、`AcpNotification`(5)、`ReverseInteractionKind`(2) |
| `crate::app` | 49 | — | — | `panel_types::PanelKind`(36)、`setup_wizard::{needs_setup, SetupWizardState}`(4)、`service_registry::ProcessResourceMonitor`(2)、`App`(1) |
| `crate::config` | 44 | — | — | `save_effective`(13)、`PeriConfig`(11)、`TuiConfig`(5)、`Profiles`/`ProviderConfig`/`ConfigSource`/`AppConfig`(各 1~3) |
| `crate::components` | 30 | — | — | `textarea::TextAreaState`(12)、`wrap_text`、`render_multiline_with_cursor`、`spinner::{animation,verb}` |
| `crate::truncate` | 15 | — | — | `truncate_by_width`(11)、`wrap_by_width`(2)、`summarize_input`(1) |
| `crate::launch` | 2 | — | — | `TuiLaunchOptions` / `build_app_and_acp` / `teardown_app`（均在 `kit/entry.rs`） |
| **合计** | **365** | **210**（99 个非测试文件） | 126 | |

按子目录看，耦合集中在三处：`panels/`（72）、散文件（102，其中 `entry.rs` 11、`atoms.rs` 8）、
`message_area/`（82，几乎全是 `crate::i18n`）。`service_snapshot/`、`steer_queue/` 为 0。

#### (b) `peri-tui 其余部分 → kit`（反向边）

`crate::kit` 共 **11 个文件、77 处**引用：

| 符号 | 引用数 | 引用方 |
| --- | ---: | --- |
| `kit::atoms::*`（`ACTIVE_SESSION_ID` 12、`HITL_PENDING` 5、`VIEW_MODELS` 4、`FOLD_OVERRIDES` 4、`BRIDGE_RESET_COUNTER` 4、`init_atoms` 3、`CONFIG_SOURCE_HANDLE` 3、其余各 1~2） | 55 | `acp_client/client/{session,workspace,requests,interaction}.rs`、`i18n/mod.rs`、`config/mod.rs`、`app/mod.rs`、`app/setup_wizard/mod.rs` + 测试 |
| `kit::session_boundary::project_session_boundary` | 13 | `acp_client/client/session.rs`(10)、`client_reverse_test.rs`(2)、`workspace_test.rs`(1) |
| `kit::steer_state::{STEERS, establish_session_snapshot}` | 3 | `acp_client/client/{requests,session}.rs` |
| `kit::thread_load_consumer::{ThreadLoadRequest, ThreadLoadDispatcher}` | 2 | `acp_client/client/` |
| `kit::acp_events::request_bg_task_snapshot` | 2 | `acp_client/client/session.rs` |
| `kit::acp_types`、`kit::acp_bridge::spawn_acp_bridge_observed_with_client` | 各 1 | `app/`、`acp_client/` |

**这是四条真实环**：`kit ↔ acp_client`（主环）、`kit ↔ app`、`kit ↔ config`、`kit ↔ i18n`（各 1~2 处）。
同 crate 内模块环是合法的，跨 crate 不合法 —— **拆分的全部难度都在这里，而不是在"搬 259 个文件"本身**。

#### (c) `kit → workspace 其它 crate`

| crate | 引用数 | 文件数 | 非测试引用点 | 关键符号 | 在关键路径上的位置（报告 §1.2） |
| --- | ---: | ---: | --- | --- | --- |
| `peri_acp_types` | 89 | 49 | — | `event_data::*`、`workspace::{ReadOnlyAdmission, SessionRestoreWarning}`、`goal::GoalStatus`、`system_reminder::*`、`messages::MessageContent`、`session::{UserInput, UserInputQueueSnapshot}`、`permission::*`、`compact::CONTINUATION_HINT`、`command::UiCommandSpec` | 链首（48.6→53.4s） |
| `peri_theme` | 91 | 61 | — | `atoms::{THEME_ATOM, PALETTE_ATOM, init_theme_atoms}`、`bridge::ThemeDefinitionExt`、`component::{InputTokens, StatusBarTokens}`、`loader::*`、`semantic::SemanticTokens` | 独立小 crate（依赖 ratatui/ratatui-kit） |
| `peri_time` | 168 | 48 | — | `monotonic_now`(99)、`elapsed_since`(24)、`Instant::now`(14)、`now_wall`(7) | 链首 |
| `peri_middlewares` | 44 | 5 | **23** | `plugin::{load_known_marketplaces, save_known_marketplaces, load_installed_plugins, MarketplaceManager::extract_name, MarketplaceSource, KnownMarketplace, marketplaces_cache_dir, marketplace::find_marketplace_json, parse_marketplace_input, save_claude_settings_enabled_plugins}`、`mcp::{McpClientPool, McpInitStatus, OAuthStatus, ClientStatus}` | **中段关键路径（66.0→86.9s）** |
| `peri_acp` | 31 | 19 | **8**（7 文件） | `event::AcpEvent`(2)、`transport::types::AcpError`(4)、`provider::config::{ProviderConfig, ProviderModels}`(3) | 链尾（80.7→88.3s） |
| `peri_mcp_cron` | 3 | 3 | **2** | `CronScheduler`（`atoms.rs:502` 的 `OnceLock<Arc<Mutex<CronScheduler>>>` 静态类型 + `service_snapshot.rs:54`） | 经 `peri-agent`，约 66s 后 |
| `peri_resources` | 1 | 1 | 0（仅测试） | `sessions::SqliteThreadStore`（测试） | 53.4→62.9s |
| `peri_config` | 0 | 0 | 0 | —（Cargo.toml 声明但 kit 未直接引用，走 `crate::config` 门面） | — |
| `agent_client_protocol` | 3 | 2 | — | — | 外部 crate |

**关键观察**：`peri_middlewares` 与 `peri_acp` 的非测试引用点总共只有 **31 处、11 个文件**，
且分布集中（plugin 面板 3 个文件 + `service_snapshot.rs` + `atoms.rs` 一行静态类型 + 3 个 ACP 类型族）。
**这 31 处正是 C1 高收益版本的唯一杠杆**。

#### (d) `kit → 第三方 crates`（渲染/终端栈）

`ratatui` 0.30.2（`unstable-rendered-line-info` / `unstable-widget-ref`）、`ratatui-kit` 0.10.2（`full`）、
`ratatui-kit-markdown` 0.3.0、`ratatui-image` 11.0.6（`crossterm`，**禁用** `image-defaults` 与 `chafa-dyn`）、
`image` 0.25（仅 png/jpeg/gif/webp）、`png` 0.18、`pulldown-cmark-012`、`syntect`（`default-fancy`）、
`unicode-width`、`unicode-segmentation`、`fluent`/`fluent-bundle`/`unic-langid`、`im`、`fuzzy-matcher`、
`arboard` + macOS `objc2`/`objc2-app-kit`（剪贴板）、`tokio`/`tokio-util`/`parking_lot`/`chrono`/`uuid`/
`reqwest`/`serde_json`/`sysinfo`/`dirs-next`/`rand`/`thiserror`/`tracing`/`anyhow`/`async-trait`/`base64`。

这些依赖全部是 **kit 专属**（非 kit 部分只用其中少数），出库后自然随 kit 走；`peri-tui` 宿主层可相应瘦身。

#### (e) kit 内部枢纽（决定"能不能切小块"）

按"被其它模块引用次数"排序：`atoms`(135，其中 6 个子目录/散文件组都引用它)、
`tui_render_unit`(61)、`panel_registry`(31)、`acp_types`(30)、`panel_mouse`(23)、
`list_nav`(20)、`message_area`(19)、`panel_scroll`(18)、`popup_overlay`(15)、`stream_data`(15)。

`atoms` 是唯一的跨领域状态枢纽：**849 行、152 个 pub 项、135 条入边**，且自身反向引用
`app::panel_types`、`app::setup_wizard`、`acp_client`、`config`。

### 1.3 编译成本位置

来自诊断报告（同一台机器）：

| 项 | 数值 |
| --- | --- |
| clean dev 全量 | 98.2s（650 单元，累计 CPU 863s，平均并行度 8.8/18 核） |
| workspace 内部串行链 | 49.4s（48.6→98.0s，并行度 1~3） |
| **peri-tui 在链上的段** | **13.4s**（lib 9.4 + bin 4.0）—— **全链终端** |
| peri-tui frontend / codegen | 5.7s / 3.7s（frontend 占 60.6%） |
| 链上其它段 | acp-types 4.8 → resources 9.5 → agent 6.8 → **middlewares 20.9** → acp 7.6 |

本 worktree 主仓库 `target/cargo-timings/` 中留存的 3 份**增量** timing 提供了一组交叉验证
（`todo` 模式单元，非 clean）：

| 单元 | frontend | codegen | 合计 |
| --- | ---: | ---: | ---: |
| `peri-tui` lib（3 次采样） | 2.38 / 2.49 / 2.47 | 0.66 / 0.89 / 1.02 | 3.04 / 3.38 / 3.49 |
| `peri-tui` bin（3 次采样） | — | — | 1.37 / 2.07 / 1.80 |

结论：**peri-tui 是依赖图的终端叶子**（无任何 crate 依赖它），其全部编译成本都在收尾段串行付出。
把叶子切成两段是**串联**，串不出时间；只有切成**并列**（两个互不依赖的兄弟 crate）才可能回收时间。

> **数据一致性缺口（需复测）**：报告 §1.2 给出 `peri-tui 84.7→94.0`，但静态依赖图（各 `Cargo.toml`）
> 显示 `peri-tui → peri-acp`，而 `peri-acp 80.7→88.3` —— 子单元不可能在依赖完成前启动。
> 该时间线重建存在约 4s 的内部不一致。本文所有收益推演因此只做**量级判断**并显式标注不确定度；
> 实施前后必须以 §六 的方法重新采集一次干净关键路径。

---

## 二、拆分边界候选

### 方案 A：整体 `kit/` 出库（保持现有调用方向）

**切面**：`kit/**` 整体 → `peri-tui-kit`；宿主的 `entry.rs` 留在 peri-tui。
**依赖形态**：`peri-tui-kit ← peri-tui(lib) ← peri`（三段串联，比今天多一段）。

**必须处理的耦合点**：

1. `kit → crate::{i18n, components, truncate, app::panel_types, app::setup_wizard}`——
   这些是"共享叶子"，因为 kit 与宿主都用，**只能一起下沉或上移**；
2. 四条环 `kit ↔ {acp_client, app, config, i18n}`——必须先在**单 crate 内**完成端口反转
   （见方案 C 的前置清单），否则新 crate 编译不过；
3. `kit → peri_middlewares`（23 处非测试引用）——原样带走，即 kit 仍然被钉在
   20.9s 的 `peri-middlewares` 之后。

**预计编译收益**：**≈ 0，甚至小幅为负**。
机理：今天终端段是 `lib 9.4s + bin 4.0s = 13.4s`；拆后变成
`kit ≈ 9.4×0.836 ≈ 7.9s` + `lib(宿主残部) ≈ 11.4k 行 ≈ 1.0s` + `bin/link 4.0s ≈ 12.9s` —— 差异在测量噪声内。
若切分不当（例如 `entry.rs` 留在 lib 导致 lib 仍依赖 kit），串联只增不减。
**不确定度：低**（这是依赖图拓扑决定的，不是测量问题）。

**风险**：低（结构不变）。**工作量**：最大（86k 行迁移 + 全部环反转），**收益**：最小。
**结论：不推荐单独实施。**

### 方案 B：先切"宿主无关"的内聚子集

**切面设想**：只搬不引用 `crate::{app, acp_client, config, i18n, components, truncate, launch}` 的模块。

**实测否决**：做传递闭包（节点 = 子目录 + 散文件；边 = `crate::kit::X` / `super::X`）后，

- 直接无宿主引用的节点 46/98，合计 11,101 行；
- **严格闭合（所有 kit 内部依赖也在子集内）后只剩 20 个节点、3,833 行**，占 kit 的 **4.5%**，
  且其中 78% 是测试文件（`steer_state_test` 511、`image_safety_test` 359、`image_preview_test` 191、…）；
- 即使先把 `i18n`(238) + `components`(2,588) + `truncate`(789) 下沉进 kit，闭合子集也只增加到
  **3,998 行**：被挡住的正是 `markdown`(4,955，仅差 1 条 `message_area` 边)、`panels`(15,230)、
  `message_area`(14,531)、`acp_events`(3,847) 这些大件。

原因很清楚：几乎所有 kit 模块都经由 `atoms` / `tui_render_unit` / `acp_types` / `panel_registry`
这几个枢纽间接摸到宿主模块。

**结论：方案 B 不成立——不存在"先切一小块"的捷径。** 这个否认性结论本身是本次分析最有价值的结果之一：
它把 C1 的讨论从"搬哪些文件"强制拉回到"反转哪些依赖"。

### 方案 C：底座下沉 + kit / host 并列 + 端口反转（**推荐**）

**切面定义**（三层，箭头为编译期依赖方向）：

```text
        peri-acp-types / peri-theme / peri-time            (既有底层契约)
                       │
              peri-tui-core      (~8~9k 行，新增)
     atoms 底座 · i18n · components · truncate · panel_types
     tui_render_unit · acp_types(视图类型) · image_safety
     text_util · terminal_caps · steer_state · session_boundary
                     ┌─┴─┐
      peri-tui-kit ──┘   └── peri-tui-host
      panels · message_area   app · acp_client（pump/RPC）·
      markdown · acp_events   config · launch · cli_* · truncate 宿主侧
      acp_bridge/notifier ·   （~13k 行）
      popups · input_area ·
      setup_wizard（~77k 行）
                     └─┬─┘
                       peri (bin: main.rs + kit::entry::run_kit_fullscreen + cli_*)
```

**三条并列关系**是收益来源：`kit` 与 `host` 互不依赖，可在 `core` 之后同时编译；只有 bin 等两者。

**需处理的耦合点（可穷举，共 4 组环 / 约 10 个反转点）**：

| # | 环 | 现状 | 反转动作 |
| --- | --- | --- | --- |
| 1 | `kit ↔ acp_client` | kit 用 `InteractionOwner`/`InteractionUiOutcome`/`ReverseInteractionKind`/`AcpNotification`/`AcpTuiClient`；acp_client 反写 `kit::atoms`/`session_boundary`/`steer_state`/`acp_events` | 双向：① 把交互协议类型与 `AcpNotification` 下沉到 core（`acp_client/interaction_lifecycle.rs` 933 行、`interaction_response.rs`、`client.rs` 的通知定义）；② `AcpTuiClient` 抽成 core 中的 trait，pump 侧实现；③ acp_client 的 6 类状态写入改为 core 提供的**发布端口**（trait / 注册回调），不再直连 `atoms` |
| 2 | `kit ↔ app` | kit 用 `panel_types::PanelKind`(36 处)、`setup_wizard::*`、`service_registry::{ProcessResourceMonitor, SharedPeriConfig}`；app 反写 `kit::atoms::{MCP_PANEL_POOL, CONFIG_SOURCE_HANDLE}` | ① `panel_types.rs`(48 行，纯枚举) 下沉 core；② `app/setup_wizard/`(804+396 行，实为 UI) 整体上移 kit；③ `ProcessResourceMonitor`/`SharedPeriConfig` 下沉 core 或抽 trait；④ app 的 2 处状态写入走端口 |
| 3 | `kit ↔ config` | kit 用 `save_effective`(13)、`PeriConfig`(11)、`TuiConfig`(5)；config 反写 `kit::atoms::CONFIG_SOURCE_HANDLE`(2) | `config/mod.rs` 仅 35 行、是 `peri-config` + `peri-acp::provider` 的**薄门面**（`tui_config.rs` 只有 1 行 re-export）⇒ kit 直接 import 上游 crate 即可；`save_effective` 的全局句柄依赖改为 core 中的窄接口 |
| 4 | `kit ↔ i18n` | i18n 反写 `kit::atoms::LANG_VERSION`(1 处) | i18n(238 行) 整体下沉 core，`LANG_VERSION` 随之下沉，环自动消失 |

**与方案 A 的差别**：A 是"先搬后解"，C 是"先解后搬，且把解出来的共享件放到能同时服务两侧的位置"。
C 的前置工作（上表 4 组）**无论选哪个方案都必须做**，A 只是把同样的工作放在搬迁之后。

**预计编译收益**：见第三节。

---

## 三、预期收益推演与不确定度

### 3.1 两种形态

| 形态 | 终端段构成 | 推演 | 相对 98.2s | 不确定度 |
| --- | --- | --- | --- | --- |
| **C-1/2/3**（kit 仍在 middlewares 之后起跑） | `core(≈0.5s) → max(kit ≈7.0s, host ≈0.9s) → bin+link(4.0s)` | 13.4s → ≈11.5s | **约 -1~-2s（1~2%）** | 中：frontend 对行数非严格线性（泛型/宏展开/单文件规模），且报告时间线本身有约 4s 不一致 |
| **C-4**（再解耦 `kit → peri-middlewares/peri-acp/peri-mcp-cron`） | kit 起跑点从 ≈88s 提前到 ≈53s（`peri-acp-types` 完成），整段 7s 落进 48.6~88.3s 的低并行窗口 | kit 完全离开关键路径 | **再 -3~-8s（合计 4~10%）** | 高：依赖 `peri-theme`（未单独测量）、`peri-mcp-cron`→`peri-agent` 链的实际落点；**且这是 C2/C3 的收益，不应重复计入 C1** |

### 3.2 与 clean build 无关、但立刻兑现的收益

| 场景 | 现状 | 拆分后 | 依据 |
| --- | --- | --- | --- |
| 改宿主层文件（`app/`、`acp_client/`、`cli_*`、`main.rs`，约 13k 行）后 `cargo check -p ...` | 重编整个 104.9k 行单元 | 只重编 ≈13k 行单元 | 实测 `peri-tui` 单元增量 frontend **2.38~2.49s** + codegen 0.66~1.02s；按行数比例外推宿主残部为亚秒级（**外推，非实测**） |
| pre-commit `clippy`（已按 A3 收窄到改动 crate） | 改 `peri-tui/` 任一文件 → clippy 整个 104.9k 行 | 同上，只 clippy 对应 crate | 同上 |
| `--all-targets`（报告 §1.1：74.8s） | kit 的 34k 行测试与宿主测试在同一批单元内 | kit / host 的 test 单元可并行 | 机制成立，收益未测 |
| release（配合 A1 `lto=thin, codegen-units=16`） | `codegen-units=1` ⇒ 每 crate 单 CGU；peri-tui 编译 18.2s、实验值 9.2s | 拆成 2 个 crate ⇒ 2 个 CGU 可并行 | **未验证**；fat LTO 链接段（112.1s）不受影响，故本项在 A1 之后才有讨论价值 |

### 3.3 诚实标注

- **不要**引用本文的秒数做验收口径。§1.3 已指出报告时间线存在约 4s 的内部不一致；
  正确做法是按 §六 重新采集一次干净 `--timings` 再做对照。
- 现有证据只支持两个强结论：**(i)** 依赖方向不变时拆叶子不缩关键路径（拓扑结论，不确定度低）；
  **(ii)** 严格闭合的"干净子集"只有 4.5%（枚举结论，不确定度低）。
  收益秒数属于推演，**必须实测**。

---

## 四、风险

按严重度排序：

1. **收益兑现风险（最大）**：若只做 C-1/2/3 而不做 C-4，投入（86k 行迁移 + 4 组环反转 + 全部脚本/文档更新）
   换来 1~2% 的 clean build 改善，**性价比很可能不如同时期的 A1/A4/D1/D2**。诊断报告已实测 A1 使 release
   全量 -46%（-97.7s）——那是 C1 量级收益的数十倍，成本却只是两行 profile 配置。
2. **依赖门静默失效**：`scripts/import-exemptions.conf` 的 TUI 边规则以 `peri-tui/src` 为被检目录
   （第 48/49 行）。新 crate 目录（如 `peri-tui-kit/src`）**不在任何规则内** ⇒ 86k 行脱离 §0 全边依赖门检查，
   且当前 kit 侧的豁免项（`kit/atoms.rs`、`kit/panels/plugin`、`kit/service_snapshot.rs`、
   `kit/workflow_snapshot.rs`）会变成"永远不命中"的僵尸配置。这是**安全网丢失**，不是普通的配置遗漏。
3. **pre-commit clippy 映射错位**：`scripts/precommit-changed-crates.sh` 用路径前缀匹配（要求 `/` 边界，
   如 `peri-tui=peri-tui`）。若新 crate 放在 `peri-tui/kit/`，改动会被映射到 `peri-tui` 而非新包；
   若放在顶层 `peri-tui-kit/`，则需在 `MAPPING` 补一行，否则改动新 crate 时 clippy 直接跳过（脚本对无命中
   输入静默返回空）。
4. **可见性工程量大**：702 个 `pub` + 212 个 `pub(crate)` + 58 个顶层 `pub mod`。跨 crate 后
   `pub(crate)` 语义变化，kit 内部大量"crate 内私有的实现细节"会变成必须公开或必须重新包装的 API。
   这是长期维护成本，不是一次性成本。
5. **测试目标的语义变化**：34k 行测试随 kit 走。`cargo test -p peri-tui --lib`
   （`peri-tui/CLAUDE.md` §目标命令、`docs/code-index/peri-tui.md` 均以此为准）将**不再运行 kit 的测试**。
   CI 用 `--workspace` 覆盖，不受影响；但本地文档、任务指引、`e2e` 脚本里的命令锚点需要同步更新。
6. **文档/索引漂移**：全仓 `src/kit/` 路径引用共 **162 处**，集中在
   `docs/code-index/peri-tui.md`(33)、`spec/issues/2026-09-29-image-upload-mcp-readback-plan.md`(33)、
   `peri-tui/CLAUDE.md`(9) 等。`docs/code-index/peri-tui.md` 的「速查表」几乎每一行都以
   `src/kit/...` 定位（如 `src/kit/acp_bridge.rs`、`src/kit/steer_queue.rs`），拆分后全线失效。
7. **缓存全量重建**：拆分瞬间所有开发者与 CI 的 cargo 缓存失效一次（本机 clean dev 98.2s；
   CI runner 更慢）。同时 `target/debug/incremental`（7.4G，peri-tui 单项 559M）会重建。
8. **改 kit 内部文件并不变快**：拆分只让**宿主层**改动受益；改 `kit/panels/*` 仍需重编整个 kit 单元
   （≈77k 行）。若日常改动主要落在 kit 内，这项收益为零。
9. **frontend 非行数线性**：报告 §归因 4 指出 `peri-tui/src/kit/acp_events_test.rs`（目录合计 7,412 行）
   单文件规模本身推高编译量。这些大文件拆 crate 后仍在 kit 内，不会自动缓解。
10. **`--locked` / `Cargo.lock` 变更**：新增 workspace member 会改写 `Cargo.lock`；本仓库对 `--locked`
    敏感（诊断 §归因 5 记录了并发改写导致 `--locked` 随机失败），提交时需独占一次 lock 更新。

---

## 五、前置条件

拆分的**全部难度在前置条件**上；以下任一项未完成，方案 C 无法开工：

### P1. 依赖反转（阻断项，必须在单 crate 内先完成）

1. **`acp_client → kit::atoms`（55 处）端口化**：定义 core 侧的状态发布接口，pump/session 侧改为经接口写入
   （与 C3「host_ports 端口化」同构，建议合并设计）。
2. **`acp_client → kit::session_boundary`（13 处）**：`project_session_boundary` 是会话边界投影，
   随 core 下沉（它本身不依赖宿主）。
3. **`acp_client → kit::{steer_state, thread_load_consumer, acp_events}`（7 处）**：同上，随 core 下沉。
4. **`app → kit::atoms`（2 处）**：`MCP_PANEL_POOL` / `CONFIG_SOURCE_HANDLE` 是全局面板句柄，
   改为端口或把句柄容器下沉 core。
5. **`config → kit::atoms::CONFIG_SOURCE_HANDLE`（2 处）**：`save_effective` 改为接收句柄参数或经窄接口。
6. **`i18n → kit::atoms::LANG_VERSION`（1 处）**：随 i18n 下沉 core 自然消解。
7. **`kit → crate::config` 直连上游**：`config/mod.rs` 是薄门面，kit 改为直接依赖 `peri-config` /
   `peri-acp::provider`。
8. **`kit → crate::components`（30 处）/ `crate::truncate`（15 处）**：确认下沉 core（`components` 目前
   **只被 kit 使用**，`truncate` 只被 kit + `app/mod.rs` 1 处使用，均为低风险）。
9. **`app/setup_wizard`（804+396 行）上移 kit**：它是 UI 逻辑，被 kit 的 setup wizard 全量 `use ...::*`。

### P2. 归属与目录决策

- 新 crate 的目录：建议**顶层 `peri-tui-kit/`**（而不是 `peri-tui/kit/`），以保持
  `precommit-changed-crates.sh` 的前缀映射简单、且 `check-layer-imports.sh` 的目录规则可独立书写。
- `edition = "2024"`（peri-tui 与 peri-theme 均已升级，新 crate 应对齐；根 `workspace.package` 仍是 2021）。
- 复用根 `[workspace.dependencies]`；`peri-middlewares` / `peri-resources` / `peri-theme` / `peri-acp`
  目前是 **path 直依赖**（不在 `workspace.dependencies` 中），新 crate 若同样引用，需一并决定是否提升到
  workspace 级（否则 `peri-tui-kit` 与 `peri-tui` 各自声明，容易漂移）。
- 顶层 `lib.rs` 的 `#![allow(clippy::...)]` 7 条与 kit 的模块树需要重新落位。

### P3. 工具与门更新

| 文件 | 需要的改动 |
| --- | --- |
| `scripts/import-exemptions.conf` | 新增新 crate 的 TUI 边规则（`use` + 全路径两模式），把现有 4 条 kit 豁免迁移过去；**验收时必须做反向验证**（临时移除规则 → 门必须变红），防"规则写了但不命中" |
| `scripts/precommit-changed-crates.sh` | 在 `MAPPING` 中补 `peri-tui-kit=peri-tui-kit`（否则改动新 crate 时 clippy 静默跳过） |
| `.github/workflows/ci.yml` | 无需改（用 `--workspace` / `--exclude`，新 member 自动纳入） |
| `release-agent.yml` / `pre-release.yml` | 无需改（`-p peri-tui --bin peri` 不受影响） |
| `peri-tui/CLAUDE.md` | §Scope、§任务路由表、§目标命令（`cargo test -p peri-tui --lib`） |
| `docs/code-index/peri-tui.md` | 33 处 `src/kit/...` 路径锚点 + 速查表 |
| `docs/standards/architecture-contracts.md` | `ARC-BOUNDARY-001` 的 Scope 从 `peri-tui` 扩到 `peri-tui-kit`（或新增条目） |
| `docs/design/tui-acp-data-flow.md` | 「分层与所有权」表格 |

### P4. 明确不做（避免范围蔓延）

- 不在本次拆分中调整 `ratatui` / `ratatui-kit` 版本；
- 不重命名 kit 内部模块（保持 259 个文件路径不变，只改 crate 归属），把 diff 控制在
  `mod` 声明与 `use` 路径上，便于 review 与回滚；
- 不改 `atoms` 的语义（只拆归属，不重构状态模型）。

---

## 六、验证方式

### 6.1 收益验证（必须无并发干扰）

诊断报告 §归因 5 明确记录了多会话共用 target 目录对时效数据的污染。所有对照必须：

```bash
# 前置：确认无其它会话在跑 cargo（诊断 D2/D3）
ps aux | rg -v rg | rg 'cargo|rustc' || echo "clean"

# 基线（拆分前，独立 target）
CARGO_TARGET_DIR=/tmp/peri-c1-before ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --timings
# 对照（拆分后，独立 target）
CARGO_TARGET_DIR=/tmp/peri-c1-after ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --timings
```

**关键路径重建**（不要只看总时长；报告 §1.2 的时间线有约 4s 内部不一致，必须以原始单元数据重建）：

```bash
CARGO_TARGET_DIR=/tmp/peri-c1-after ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --timings
# 解析 target/cargo-timings/cargo-timing-*.html 内嵌的 UNIT_DATA（JSON 数组，
# 每项含 name / start / duration / sections[{frontend,codegen}]），
# 按 start+duration 排序即得关键路径；应输出 peri-tui-kit 的起跑/结束时刻。
```

**场景矩阵**（每项 before/after 各一次）：

| 场景 | 命令要点 | 关注 |
| --- | --- | --- |
| clean dev | 如上 | 终端段时长、`peri-tui-kit` 起跑时刻 |
| `--all-targets` | `build --workspace --all-targets` | kit 测试单元的并行度 |
| release（叠加 A1） | `--release` + `lto="thin"`+`codegen-units=16` | CGU 是否真的并行 |
| 增量·改宿主 | touch `app/mod.rs` 后 `build -p peri-tui --bin peri` | 期望 ≈ 亚秒级（当前 ≈3.0~3.5s） |
| 增量·改 kit | touch `kit/status_bar.rs` 后同上 | 期望**不变差**（这是本方案的"无收益面"） |
| `check` | `check -p peri-tui` | pre-commit 体感 |

**通过标准（建议）**：clean dev 终端段下降 ≥1.5s 且总时长不劣化；增量·改宿主下降 ≥1.5s；
增量·改 kit 劣化 <5%。任一不达标即回到方案评审，不要"先合了再说"。

### 6.2 正确性验证

```bash
./scripts/cargo-rmcp-patched.sh test --locked --workspace          # CI 等价
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib    # 宿主层
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui-kit --lib # kit 层（新增）
./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/check-layer-imports.sh          # 需先完成 P3，并做反向验证
bash scripts/check-file-size.sh
git diff --check
```

**契约层**：`ARC-BOUNDARY-001`（TUI 只经 ACP 取数、不直接驱动 agent loop）与 `ARC-EVENT-001`
的语义**不因 crate 边界改变**，但 Scope 需要扩写；`peri-tui/CLAUDE.md` 的稳定不变量
（BridgeState 是事件→状态边界、render body 不写 atom、hooks 稳定顺序、`ScrollbarHook`/`CenterBandHook`
注册次序等）全部继续适用，且**拆分不得成为修改这些行为的借口** —— 拆分提交应当只包含路径与 `mod`/`use` 变更。

**端到端**：`e2e/` 中的 TUI 场景（steer queue 实机验收、legacy history upgrade、像素渲染路径）
需要在拆分后至少跑一遍；这三类分别覆盖新 crate 边界上的输入链路、会话边界投影与图像渲染。

**反向验证（防僵尸配置）**：`check-layer-imports.sh` 的规则与豁免必须逐条交叉验证
——移除任一新规则或任一豁免，门必须变红。当前 4 条 kit 豁免在拆分后若"仍然全绿"，
说明规则写错了目录。

---

## 七、与 C4（拆 `peri-middlewares` 的 `mcp/`）的关系与排序建议

**关系：不同链段，但共享同一个前置。**

| | C1（本文） | C4（拆 peri-middlewares mcp/） |
| --- | --- | --- |
| 作用链段 | **终端段**（peri-tui 13.4s，全链最后） | **中段**（peri-middlewares 20.9s，全链最大单点，约 66.0→86.9s） |
| 对关键路径的杠杆 | 小（终端叶子） | 大（当前最大件，拆开可提升 1~3 的并行度） |
| 内部交叉耦合 | 4 组环、约 10 个反转点（§二方案 C 表） | 更重：`mcp` 被 8 个模块引用，`assembly→mcp` 20 处，`mcp↔assembly` / `mcp→plugin` / `mcp→subagent` 互引（报告 §归因 1） |
| 已登记的同类债务 | `import-exemptions.conf` 的 `TUI-fullpath` 豁免（kit/atoms、kit/panels/plugin、kit/service_snapshot） | 无现成豁免；需先做端口化设计 |
| 与既有任务线 | M-TUI（TUI 全量经 ACP）、C2/C3 | C2/C3 的下游 |

**排序建议**：

1. **先做 C2 / C3 / M-TUI 的依赖反转**（`mcp` 命令面、`plugin` 数据源经 ACP、`host_ports` 端口化）。
   这一步同时是 C1 的 C-4 步骤和 C4 的前置 —— **一次投入，两处收益**。
2. **C1 的结构阶段（C-1/2/3）可与 C4 的端口化设计并行推进**，但**不要先合**：
   在 C-4 未落地时，C1 的收益只有 1~2%（§三 3.1），而它带来的门/脚本/文档改动面很大（§四 2/3/6），
   先行合入会把"低收益 + 高摩擦"叠加在一起。
3. **优先级判断**：就"最短时间拿到最大构建收益"而言，本报告的四梯队排序仍然成立 ——
   `A1`（release -46%，已实测）、`A4`（aws-lc-sys 关键路径屏蔽实验，可能 -30s）、
   `D1/D2`（缓存与并发治理）、`A2` 复测 都**先于** C1。C1 应定位为
   **C2/C3 完成后的结构性收口**，而不是独立的性能手段。
4. **若最终不做 C1**：本文的否认性结论（§二方案 B）仍有价值 —— 它说明 kit 的耦合是
   由 `atoms` 这一个 849 行的枢纽造成的。**先治理 `atoms`**（把它的宿主依赖摘掉、把 152 个 pub 项分层）
   无论拆不拆 crate 都会有正向收益，且成本远低于整体迁移。

---

## 附录：本次统计的命令（只读，可复现）

```bash
# 分目录行数
cd peri-tui/src && for d in */ *.rs; do ...; done          # 见 §1.1 表
# kit 子目录 prod/test 拆分
python3 -c "…os.walk('kit')…"                              # 见 §1.1 表
# 依赖方向计数
rg -o 'crate::(app|acp_client|components|config|i18n|launch|thread|truncate|update|alloc_config)\b' -g 'kit/**/*.rs' --no-filename .
rg -o 'crate::kit::[a-z_0-9]+' -g '!kit/**' --no-filename .
rg -o 'peri_middlewares::[A-Za-z_0-9:]+' -g 'kit/**/*.rs' --no-filename . | sort | uniq -c | sort -rn
# kit 内部枢纽与闭合子集（§1.2(e) / §二方案 B）
python3 -c "…节点=子目录+散文件，边=crate::kit::X / super::X，迭代求闭包…"
# 编译耗时交叉验证（本 worktree 主仓库 target/cargo-timings/）
python3 -c "…正则提取 html 内 const UNIT_DATA = [...] 后 json.loads…"
```

**上游数据**：`spec/issues/2026-10-05-rust-build-performance-diagnosis.md` §1.1–§1.4、§二归因 1、§四候选清单。
