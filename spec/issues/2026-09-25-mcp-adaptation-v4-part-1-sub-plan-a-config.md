# MCP adaptation v4-part-1 — sub-plan A：配置契约

> **主 plan v2 覆盖（优先于本文）**：见 [`2026-09-25-mcp-adaptation-v4-part-1-plan.md`](2026-09-25-mcp-adaptation-v4-part-1-plan.md) §5。本文与主 plan 冲突处一律以主 plan 为准，涉及本文件的具体覆盖：**R3**（A-04 不再拥有任何 `docs/code-index/` 文件）、**R4**（`system_mcp_timeout` 归 A，本文 IF-A5「B 决定 timeout、A 不新增字段」作废）、**R5**（`McpServerConfigValidationError` 为三变体，本文 IF-A2 的单变体作废）、**R7**（插件严格化收窄为 MCP 专用路径，本文 IF-A4 的整体严格化表述作废）、**R14**（A-02 拆为 A-02a 机械收口 / A-02b 错误闭环）、**R2**（`initialize.rs` 的 `unwrap_or_default()` 修复归 B-02，不归 A）。

## 1. 元信息

- 日期：2026-09-25。状态：实现规划，未实施、未运行构建或测试。
- 唯一目标事实源：已完整读取 `docs/design/mcp-adaptation-v4-part-1.md:1-244`。本计划覆盖验收契约 **1**、**3 的配置无损传递部分**、**4 的配置语义部分**；文档表述遵守契约 **7**。
- 本次 workflow 仅实施契约 1–4、7；契约 5、6 不是本次迁移实施范围，但不允许破坏已有安全、生命周期契约。
- 依赖关系：A 提供 B/C 的字段与错误接口；B 负责 1R 启动准入、ready/timeout/失败传播；C 负责所属 namespace 的逐工具解析、schema/bridge、direct 注入及空列表不注入的运行时证明。A 单测通过不等于契约 2–4 已完整验收。
- 可验证产物：非法字段组合在直接 serde、项目、全局、插件来源均被拒绝且错误可追踪；失败不被合并成成功的空配置；禁用/删除配置不绕过校验；有效工具数组经合并、展开、写回保持值与顺序；`Some([])` 与未声明保持可区分。
- 本轮唯一写入文件即本文；下文目标文件和命令都是后续实施计划，不是本轮写入授权。

## 2. 事实基线

### 2.1 权威语义与实际字段

- 设计 `:35-40`：`system_mcp=true` 是启动依赖；未标识/false 是普通 MCP。`:45-65`：工具名数组、所属 namespace、缺失必须失败、空数组只要求 ready。`:226-232`：7 条验收契约及目标/现状区分。
- `peri-acp-types/src/plugin.rs:41-77::McpServerConfig` 是唯一字段定义；`peri-middlewares/src/mcp/config.rs:9-12` 仅 re-export，不应另定义平行 DTO。

| 当前字段 | 类型 | 实际 serde 属性 / 缺省行为 |
| --- | --- | --- |
| `command` | `Option<String>` | 无字段属性；缺失为 None |
| `args` | `Option<Vec<String>>` | `default` |
| `env` | `Option<HashMap<String, String>>` | `default` |
| `url` | `Option<String>` | 无字段属性；缺失为 None |
| `headers` | `Option<HashMap<String, String>>` | `default` |
| `oauth` | `Option<OAuthConfig>` | `default` |
| `disabled` | `Option<bool>` | `default, skip_serializing_if="is_false"`；`:111-113` 的 helper 将 None/Some(false) 都视为可省略 |
| `protocol_version` | `Option<McpProtocolVersion>` | `default, rename="protocolVersion", skip_serializing_if="Option::is_none"` |
| `subscriptions` | `Option<McpSubscriptionsConfig>` | `default, skip_serializing_if="Option::is_none"` |
| `source` | `Option<ConfigSource>` | `skip`；运行时来源，不进入 wire |

- **注意纠正一个容易误读的前提**：`McpServerConfig` 本身没有 `rename_all="camelCase"`。该属性实际在 `config.rs:14-18::McpConfigFile`、`plugin.rs:84-98::McpSubscriptionsConfig`、`:116-130::OAuthConfig`；`protocolVersion` 确实是单字段 `rename`。新增 key 不需要改变全结构命名策略。
- `plugin.rs:142-179::McpServerEntry` 已手写 Deserialize，内联对象最终调用 `serde_json::from_value::<McpServerConfig>`，保留 `serde::de::Error::custom`；字符串是文件引用。

### 2.2 加载、合并、写入和消费入口

| 实际入口与定位 | 当前行为 / 对实施的意义 |
| --- | --- |
| `peri-middlewares/src/mcp/config.rs:45-56::load_from_path` | 缺文件成功返回空配置；读取/serde 错误分别返回 ReadError/ParseError。类型级 Deserialize 校验可覆盖这里。 |
| 同文件 `:60-86::load_global_config` | 先取 `config.mcpServers`，否则顶层 `mcpServers`；`:84` 对服务器 map 反序列化 `unwrap_or_default()`，会吞掉非法配置，必须改为错误传播。 |
| 同文件 `:227-348::load_merged_config_full` | 实际全局位置由 home/.peri/settings.json 决定，`claude_home` 只用于插件；`:238-245`、`:296-303` 将全局/项目失败变为空配置。合并优先级 global < plugin < project；校验必须先于覆盖与去重，不能让非法低优先级输入被覆盖后消失。 |
| 同文件 `:250-291` | 调用宽容的插件聚合 API；为 server key 加 `plugin:{name}:{server}` 前缀、展开插件变量、注入插件 env 并记录 marketplace。不得在此改写工具名数组。 |
| 同文件 `:89-114::server_config_hash`、`:308-323` | hash 当前只处理 command/args/env/protocol_version；去重可删掉插件服务器。新增启动依赖字段必须参与 hash，且 System MCP 不应因跨 namespace 内容去重而丢失要求。 |
| 同文件 `:178-212::expand_server_config_with_context` | 逐字段重建 struct；新增字段若不显式复制会编译失败，若误做变量展开会损失工具名。 |
| 同文件 `:353-354::load_merged_config` | 公开 API 当前返回 McpConfigFile，无 Result；只取 full 的 `.0`。需要显式变更为可失败 API，不能保留返回空配置的兼容壳。 |
| 同文件 `:398-472::remove_server_from_config_with_paths` | 项目文件先解析为 McpConfigFile；全局只操作 Value，分别尝试 nested/top-level，成功后 atomic write。全局支路尚无类型校验。 |
| 同文件 `:490-579::set_server_disabled_with_paths` | 项目与全局均直接修改 Value 再写；全局找不到目标也可能重写文件。新增校验不能只放在 serde 类型化写回路径。 |
| `peri-middlewares/src/plugin/config.rs:492-506::load_plugin_manifest` | 读取 `.claude-plugin/plugin.json` 并反序列化为 PluginManifest，内联 MCP 会经过 McpServerEntry。 |
| `peri-middlewares/src/plugin/loader.rs:402-433::load_mcp_json_file` | 支持 wrapped/flat；`.ok()`、`if let Ok` 丢失错误；wrapped 解析失败还会尝试 flat。不能把非法 server 当作可跳过条目。 |
| 同文件 `:438-488::extract_mcp_servers` | 内联对象 clone；文件引用加 `entry.server` 前缀；当前以 `result.is_empty()` 触发根 `.mcp.json` 回退，不完全等同注释“manifest 未声明”。严格 MCP 路径必须按“未声明”决定 fallback，而非按解析失败决定。 |
| 同文件 `:491-537::load_plugins` | manifest 错误触发 synthetic fallback；再次失败则 continue。非法 MCP 不能被修复回退掩盖或当作未安装。 |
| 同文件 `:563-592::load_enabled_plugins`、`:632-647::load_enabled_plugins_aggregated` | 前者返回 Result，后者错误变成无诊断的空 PluginLoadResult。`:612-623` 仅 clone 加 namespace。执行加载必须离开这个宽容聚合通道。 |
| `peri-middlewares/src/mcp/initialize.rs:21-39::run_initialize`、`:346-371::initialize` | 两处直接解构合并结果；前者生产、后者 cfg(test)。`:60-69` 空配置发布 Ready；`:82-85` 把原 config clone 到 pool。错误变空会伪造 Ready；必须在这里将错误保留为 Failed，且在禁用过滤前校验 typed 配置。 |
| `peri-middlewares/src/mcp/transport.rs:30-46::TryFrom<&McpServerConfig>` | 当前仅检查 command/url；公开 Rust struct 可手工构造，所以 Deserialize 不是唯一闸门。transport 转换也要调用同一纯校验方法。 |

其他持久化入口也已检索，不误称只有 config.rs 写磁盘：

- `peri-middlewares/src/plugin/installer/mod.rs:139-147::generate_synthetic_manifest` 将 marketplace 的 `mcpServers` 原样拷入 manifest；严格加载必须验证生成后 manifest，不允许以生成成功替代配置合法。
- `peri-tui/src/sync/writer.rs:124-164,177-203::write_sync_items` 可原样同步 settings、MCP、插件文件；`peri-tui/src/sync/channel_flow/staging.rs:79-97` 可暂存 `.mcp.json`。它们是文件搬运，不发布 MCP 配置/ready。本计划不要求禁止磁盘上出现非法字节（手动编辑同样可以）；要求每次载入/使用这些字节必经严格配置入口。不得为此另扩展整个同步子系统。
- `peri-acp/src/host/requests/plugin.rs:26-32` 明确插件 MCP 修改不触发池刷新，需下次装配/会话重启；热重载不是本计划承诺。

### 2.3 动态路径核实

- `peri-middlewares/src/mcp/dynamic/tool.rs:259-291::bind_invocation`：`DynamicMcpAction::from_tool_input(input)?.canonicalize()?`，不是 McpServerConfig。
- `peri-acp-types/src/dynamic_mcp.rs:119-140::DynamicMcpConfig` 有 `rename_all="camelCase", deny_unknown_fields`；字段为 command/args/env/cwd/url/headers/timeout_ms/protocol_version/subscriptions，**没有**两个 System 字段。`:157-166::CanonicalDynamicMcpConfig` 也没有；`:197-198` 默认动态 timeout 为 30,000ms。
- `dynamic/registry/load.rs:12-16,48-53,71-82` 接受 CanonicalDynamicMcpLoadRequest 并储存 canonical config；`dynamic/registry/connector.rs:69-85` 调 `prepare_single_server`；`dynamic/staged_connection.rs:411-447` 直接使用 canonical transport/timeout，不经过静态 struct。
- 结论：现有动态 wire 会拒绝 System key（未知字段），不是静态校验的漏网通道。本计划保留拒绝，不新增 session 中途声明“启动依赖”的语义；增加回归保护。

### 2.4 错误、测试、API 与文档

- `peri-middlewares/src/mcp/config.rs:23-42::McpConfigError` 只有 ParseError、ReadError、WriteError，携带 path 和具体 source，无 `non_exhaustive`。
- 对仓库 Rust 源码精确检索 `McpConfigError`：除定义/构造/返回签名/re-export 外，仅 `config_test.rs:60` 有 `matches!(..., Err(McpConfigError::ParseError { .. }))`；未发现枚举的穷尽 match。该 matches 宏有隐含 false 分支，新增变体不会使此处非穷尽。未能据此保证仓库外消费者不受影响。
- `TransportError` 的对应检索仅发现 `transport_test.rs:74` 的 `matches!(..., InvalidConfig)`，没有穷尽错误枚举 match。
- `config_test.rs:1-24,56-85`：`use super::*`、普通 `#[test]`、NamedTempFile、断言 Result/字段；`:228-260` 使用 tempdir 并回读文件；`:386-484` 用真实插件目录、manifest、installed_plugins、enabledPlugins 测试合并。当前 full tests 未注入全局路径，会读取真实 home 配置，新增/改造测试应使用显式路径 seam。
- 搜索 `McpServerConfig {` 的文件：`peri-acp-types/src/plugin.rs`、`peri-middlewares/src/mcp/{config.rs,config_test.rs,client_test.rs,resource_cache_test.rs,transport_test.rs}`、`peri-middlewares/src/plugin/loader_test.rs`。新增 public 字段会破坏完整 struct literal；必须同步，而不改无关 fixture 语义。
- `peri-acp-types/src/lib.rs:1-2,31-32,62`、该 crate `Cargo.toml:5,13-16`：跨层契约位置明确；serde/serde_json/thiserror 已有依赖。校验是纯数据不变量，不把文件 I/O、home 定位、插件发现、transport/ready 编排搬进契约层。
- `docs/standards/architecture-contracts.md:54-58::ARC-TOOLS-001` 约束真实工具视图与 direct/deferred；`:122-126::ARC-SECRET-001` 禁止错误/日志泄漏凭据。没有找到一条专门规定“给 peri-acp-types struct 加字段必须使用某 feature gate”的规则，不能补造这种要求。
- `spec/issues/2026-07-22-p1-6-api-stability-compile-enforce.md:10-14,18-36,86-118`：讨论的是 peri-agent API 分级，feature gate 阶段仍 Open；不是已实施的契约层强制门禁。本计划保持既有 public 类型/re-export identity，记录公开返回类型变化和 struct literal 源码兼容性，显式迁移工作区调用者，不顺便实施该 issue。
- `CLAUDE.md:39-41,70` 要求核实 code-index 并同步、按 DOC-UPDATE-001 检查路由；`docs/standards/documentation.md:27-31,45-49` 要求更新受影响单一事实源，参考资料不得冒充权威。
- `docs/reference/README.md:6` 只路由 MCP 生态文档；目录检索没有 `mcpServers`/`McpServerConfig`/`protocolVersion` 配置参考页。`docs/reference/mcp-ecosystem.md:3-5,546,564` 有接入与现状章节，应补充简短配置说明和权威设计链接，不另建平行配置手册。
- `docs/code-index/peri-middlewares.md:39,49-50,111` 有 MCP 配置/插件入口条目；`docs/code-index/peri-acp-types.md:8,109-116` 有契约层定位但 plugin 仅在“其他”中列举；实施后分别更新严格加载入口、新字段与校验定位。

## 3. 接口冻结（Interface Freeze）

### IF-A1：字段、wire key、默认值

在 `peri_acp_types::plugin::McpServerConfig` 增加以下 public 字段；既有字段及 re-export 保持：

```rust
#[serde(default, rename = "system_mcp", alias = "systemMcp", skip_serializing_if = "is_false")]
pub system_mcp: Option<bool>,
#[serde(default, rename = "system_mcp_tools", alias = "systemMcpTools", skip_serializing_if = "Option::is_none")]
pub system_mcp_tools: Option<Vec<String>>,
```

- **冻结决策：输出严格使用设计的 snake_case key；输入额外接受 camelCase 别名。** 理由：设计字面 key 为 canonical；别名兼容周边 camelCase 使用习惯，并防止拼成 camelCase 后被现有“忽略未知字段”行为悄悄降级为普通 MCP。两种拼法同时出现必须报 duplicate field，即使值相同也拒绝。
- 不给整个 McpServerConfig 添加 rename_all 或 deny_unknown_fields，不改变其他历史 key 的解析政策。
- `system_mcp` 缺省 None，消费判定固定为 `config.system_mcp == Some(true)`。false/None 写回可省略；显式 null 对新字段拒绝，不作为未配置。
- `system_mcp_tools` 缺省 None；显式 `[]` 是 Some(vec![])，必须写回为 `"system_mcp_tools": []`，禁止 `Vec::is_empty` 省略；null、非数组、非字符串元素一律解析失败。
- Deserialize 改为私有 wire helper + 手写转换，Serialize 保持 derive。helper 的两个新字段各使用 `default` 与拒绝显式 null 的 `deserialize_with`（bool / Vec<String> 正常反序列化后包 Some），仅字段缺失时走默认 None。复制其余字段的现有 serde 属性，source 仍 skip。新 alias 的 duplicate detection 由 helper derive 保留，禁止先转 Value 导致重复 key 丢失。
- 不 trim、不排序、不去重、不展开 `${...}`、不加 MCP 前缀、不把数组拼成字符串；同名工具属于各自 server key。空字符串/重复工具等运行时可解析性由 C 决定，A 不新增设计未要求的工具名限制。
- true + None 与 true + Some([]) 在消费语义上都是只要求 ready，无必需工具；仍保存结构区别。false/None + Some([]) **非法**，不能用列表非空判断是否“声明”。

### IF-A2：纯校验及错误

定义位置：`peri-acp-types/src/plugin.rs`。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpServerConfigValidationError {
    #[error("system_mcp_tools requires system_mcp = true")]
    SystemMcpToolsRequiresSystemMcp,
}

impl McpServerConfig {
    pub fn validate(&self) -> Result<(), McpServerConfigValidationError>;
}
```

实现规则唯一为 `self.system_mcp_tools.is_some() && self.system_mcp != Some(true)` 时返回该变体；其余 Ok。方法无副作用，无 namespace/transport/I/O。Deserialize 调用 validate，错误通过 `serde::de::Error::custom` 保留上述固定正文。

- 私有 helper 精确签名（同文件）：`fn deserialize_present_system_mcp<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error>`；`fn deserialize_present_system_mcp_tools<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>`。它们分别调用 bool / Vec<String> Deserialize 后 map(Some)，因此显式 null 拒绝。
- 配置层保留现有 ParseError 文案 `MCP 配置文件解析失败: {path}: {source}`；非法 wire 组合返回 ParseError，source 正文含固定契约错误，可有 serde 行列后缀，不冻结行列数字。
- 在 `peri-middlewares/src/mcp/config.rs::McpConfigError` 增加 `InvalidServer { server_name: String, source: McpServerConfigValidationError }`，source 标记 `#[source]`，Display 固定 `MCP 服务器配置无效: {server_name}: {source}`。它用于手工构造 typed 配置校验，不把错误定义反向依赖到 middlewares。
- 同枚举增加 `PluginLoadError { source: crate::plugin::loader::LoaderError }`，source 标记 `#[source]`，Display `插件 MCP 配置加载失败: {source}`。LoaderError 不反向持有 McpConfigError，避免递归错误类型。
- `TransportError` 增加 `InvalidSystemConfig(#[from] McpServerConfigValidationError)`，Display 采用 `#[error(transparent)]`；TryFrom 首行调用 validate，保留原 InvalidConfig 的 command/url 语义。
- 错误中只保留文件定位、server 标识和固定规则文本，不打印整个配置、env、headers、URL 认证信息或 OAuth 值。测试不得用真实 secret。

### IF-A3：严格加载与失败闭环

`peri-middlewares/src/mcp/config.rs` 冻结签名：

```rust
pub fn load_merged_config(cwd: &Path, claude_home: &Path)
    -> Result<McpConfigFile, McpConfigError>;
pub(crate) fn load_merged_config_full(cwd: &Path, claude_home: &Path)
    -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError>;
pub(crate) fn validate_config(config: &McpConfigFile) -> Result<(), McpConfigError>;
fn load_merged_config_full_with_paths(cwd: &Path, claude_home: &Path, global_path: &Path)
    -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError>;
```

- validate_config 按 server name 排序调用每个 cfg.validate，首个错误稳定返回 InvalidServer；不能跳过 disabled 条目。B 对直接传入的 typed config 在任何 empty/disabled/ready 分支前调用它。
- full 委托 with_paths，生产仍按既有 home 路径；测试显式传 tempdir 下的全局路径，不改 HOME、不碰用户文件。
- load_from_path/load_global_config 原签名不变；所有 serde 错误返回 ParseError，现存文件解析失败不得 default。全局读取优先级仍 nested > top-level；如果两个 map 都存在，两者先校验，选择仍按原优先级，避免写入口操作到未经验证的备用 map。
- 全部被选择加载的 global/plugin/project 输入先验证，再覆盖/去重；缺文件仍可空，非法文件不是缺文件。合并输出再次 validate_config。
- 写入口在**修改前**验证所有相关 server map，修改后的待写结果再验证。失败不调用 atomic_write_json、不改任何字节；不能用 disabled=true 绕过。删除非法条目也拒绝（与当前项目分支先 typed parse 一致），需用户先修复文件；不引入“删除即修复”特殊权限。
- `expand_server_config_with_context` 复制 system_mcp、clone system_mcp_tools。hash 增加两个新字段；System MCP 配置不参与跨来源、跨 namespace 的内容去重删除，保留各 server 所属 namespace。同一 server key 的显式优先级覆盖仍是整条配置替换，不跨来源拼接工具数组。
- `initialize.rs` 两处接收 Result：Err 时记录/发送 `McpInitStatus::Failed(error.to_string())` 并返回，不 mark_initialized、不发布空 Ready、不开始连接；这是 A 的配置错误接线，不实现 B 的 ready 等待策略。B 必须在 1R 消费该失败，而非仅日志后继续。

### IF-A4：插件路径的严格/展示边界

`peri-middlewares/src/plugin/loader.rs` 新增执行专用入口：

```rust
pub(crate) fn load_enabled_plugins_for_mcp(
    claude_dir: &Path,
    cwd: Option<&Path>,
) -> Result<Vec<LoadedPlugin>, LoaderError>;
```

- MCP 合并唯一调用该严格入口（本次保持当前 `cwd=None` 的插件选择规则，不顺带改变启用范围）。它复用 installed/enabled 选择与插件装配逻辑；内部严格/宽容调用共用解析函数，禁止复制两份配置解析器。
- 将 `load_mcp_json_file` 的内部返回冻结为 `Result<Option<HashMap<String, McpServerConfig>>, LoaderError>`：不存在可 None，存在而非法必须 Err；wrapped 一旦出现就不尝试 flat；flat 任一 entry 非法则整个失败，不保留部分成功。
- 将 `extract_mcp_servers` 内部返回冻结为 `Result<HashMap<String, McpServerConfig>, LoaderError>`；typed 内联逐项 validate；有 manifest.mcp_servers（包括空 map）就不回退根文件。严格链保留该 Err。
- LoaderError 新增 `McpConfigInvalid { path: PathBuf, message: String }`，Display 固定 `插件 MCP 配置无效: {path}: {message}`。message 使用规则/解析位置而非原始输入值；对规则失败固定为 `system_mcp_tools requires system_mcp = true`。严格 manifest 解析同样保留明确错误；不通过匹配错误字符串决定是否跳过。
- synthetic fallback 只允许 manifest 文件不存在时发生，生成后必须严格解析；已存在但非法的 manifest 不允许覆盖修复。严格入口不能沿用 load_plugins 的 continue/aggregate 的 default。
- 现有供插件面板、skills/hooks 聚合使用的 `load_enabled_plugins_aggregated` 可保持公开返回类型，作为宽容展示路径，但必须记录安全且明确的配置错误，不能默默丢弃；不得作为任何 MCP transport/ready 的输入。当前调用点 `peri-acp/src/host/assemble.rs:75,293`、`peri-middlewares/src/host_ports.rs:94`、`peri-tui/src/launch.rs:113` 保持类型不变；本仓库 all_mcp_servers 搜索未发现这些点用于启动连接。
- **准入边界明确**：保留宽容 UI 不代表允许非法 MCP 被启动；生产 MCP 全局/插件/项目加载必须严格，且 B 在启动前消费失败。若实施中发现新增消费者用宽容聚合结果构建 MCP，则必须切换严格入口，不允许宣告验收完成。

### IF-A5：交接给 B/C 的语义

- B 判定 System 只看 `Some(true)`，不能看 tools 非空；工具为空也必须等待 ready。B 决定 timeout 配置接口，A 本次不增加 timeout 字段。
- C 输入是 `(server_key, &McpServerConfig)` 或其等价的无损 clone；仅在 `system_mcp == Some(true)` 时遍历 `system_mcp_tools.as_deref().unwrap_or(&[])`。该 slice 为空就没有额外直接注入请求，不意味着注入该 server 的全部工具。
- source 和 plugin_sources 按现有来源逻辑保留；C 负责由 server_key 解析 namespace/effective name，不从工具名猜 namespace。
- A 不定义 direct bridge 或 ready 状态；B/C 不重新定义字段、错误或独立校验规则。

## 4. 任务表

下列任务按依赖执行，各任务目标文件集合互不重叠。“独立验证”指前置任务已落地后可单独执行本行命令，不要求互相依赖的 Rust 类型变更可以任意顺序 cherry-pick。A-02 是完整的配置失败传播纵切片：签名、literal 和调用者必须一次收口，否则只是不可编译的半成品；不得再把同一文件分派给不同任务。所有命令在仓库根目录运行，**仅供后续实施，本轮未执行**。

| Task ID | 标题 | 目标文件（精确路径） | 改动摘要 | 验证命令 | 预估 diff 规模 | 与其它 task 的文件冲突面 |
| --- | --- | --- | --- | --- | --- | --- |
| A-01 | 冻结契约 DTO 与纯校验 | `peri-acp-types/src/plugin.rs` | IF-A1/A2 字段、私有 wire helper、手写 Deserialize、validate/error；同文件 cfg(test) 小型契约测试。只增加既有依赖可实现的纯逻辑。 | `cargo test -p peri-acp-types --lib -- system_mcp` | 180–260 行 | A 内无重叠；B 的 timeout 若也改此文件，必须由 A owner 合并一次，先确认 key；不能并行覆盖。 |
| A-02 | 静态配置全部入口失败闭环与无损传递 | `peri-middlewares/src/mcp/config.rs`；`peri-middlewares/src/mcp/config_test.rs`；`peri-middlewares/src/mcp/initialize.rs`；`peri-middlewares/src/mcp/initialize_test.rs`；`peri-middlewares/src/mcp/transport.rs`；`peri-middlewares/src/mcp/transport_test.rs`；`peri-middlewares/src/mcp/client_test.rs`；`peri-middlewares/src/mcp/resource_cache_test.rs`；`peri-middlewares/src/plugin/loader.rs`；`peri-middlewares/src/plugin/loader_test.rs` | 依赖 A-01。完成 IF-A2–A4；删除配置错误被当空的执行路径；严格插件入口；配置原子写前后校验；数组 clone、hash/namespace 去重保护；两处初始化错误接线；补齐完整 struct literal；增加本节与第 5 节真实文件契约测试。client/resource 文件仅补默认字段，不改测试含义。 | `cargo test -p peri-middlewares --lib -- system_mcp`；`cargo test -p peri-middlewares --lib -- mcp::config::tests`；`cargo test -p peri-middlewares --lib -- plugin::loader::tests`；`cargo check --workspace --all-targets` | 500–850 行（含回归测试及机械字段补齐） | A 内无重叠；B 可能改 initialize.rs/initialize_test.rs，A 先完成配置失败处理再交 B，不并行编辑；C 不改这些文件。若需更细提交，在同一 owner 下串行，不新增重叠任务。 |
| A-03 | 动态配置拒绝 System key 的边界回归 | `peri-middlewares/src/mcp/dynamic/tool_test.rs` | 依赖 A-02 可编译基线；通过 bind_invocation / from_tool_input 覆盖两个 snake key 和两个 alias，确认拒绝且不调用 deployment load；不改动态生产代码/DTO。 | `cargo test -p peri-middlewares --lib -- test_dynamic_mcp_rejects_system_mcp_fields` | 30–65 行 | A 内无重叠；B/C 不扩大动态生命周期，本文件仅由 A 持有。 |
| A-04 | 配置参考与索引同步 | `docs/reference/mcp-ecosystem.md`；`docs/code-index/peri-acp-types.md`；`docs/code-index/peri-middlewares.md` | 依赖 A-02。增加 canonical/alias/空数组/非法组合说明与权威设计路由；更新契约类型及严格入口索引；只标注实际验证到的配置能力，不宣称 5 MCP 迁移。 | `git diff --check -- docs/reference/mcp-ecosystem.md docs/code-index/peri-acp-types.md docs/code-index/peri-middlewares.md`；`git diff -- docs/reference/mcp-ecosystem.md docs/code-index/peri-acp-types.md docs/code-index/peri-middlewares.md`（按 DOC-UPDATE-001 人工核对） | 30–65 行 | A 内无重叠；B/C 如也需索引更新，只向此 task 提供条目，由一个文档 owner 汇总。 |

A-01 完成时 middlewares 的完整 literals 尚待 A-02 补齐，不宣称工作区可编译；A-01 自己的契约 crate 测试可独立验证。A-02 必须连带修复所有已核实调用点后再交接 B，不能把编译失败留给 B/C 猜测。

## 5. 验证计划

### 5.1 契约层（A-01）

以下函数放在 `peri-acp-types/src/plugin.rs` 的 cfg(test) module；用最小无凭据 JSON，不运行外部进程。

| 测试函数名 | 可观察断言 |
| --- | --- |
| `test_system_mcp_legacy_defaults` | 旧 JSON 解析后两个字段 None，validate Ok；输出不含新增 key，既有 protocolVersion/source 语义不变。 |
| `test_system_mcp_tools_requires_true` | 表驱动：system 缺失/false × tools 空/非空均 Err；直接 typed 构造断言 `SystemMcpToolsRequiresSystemMcp` 变体，serde 错误正文 contains 固定文案；true 两种数组成功。 |
| `test_system_mcp_empty_tools_roundtrip` | true + [] 往返后 Some(true)、Some(empty)，输出确有 snake key 和空数组；true + 缺失 tools 为 None，不能序列化为自动补充的工具。 |
| `test_system_mcp_key_aliases` | snake/alias 输入得到相同字段；输出只有 snake；同一字段两种 key 同时出现报 duplicate field；alias tools 无 true 同样失败。 |
| `test_system_mcp_rejects_null_and_wrong_types` | system null/string、tools null/string/非字符串元素均 Err；不接受为 None/空数组。只断言固定解析种类或字段上下文，不断言 serde 行列数字。 |
| `test_system_mcp_tools_preserve_exact_values` | 大小写、重复项、空字符串、含 `${VAR}` 的字面工具名及原顺序都不变；不隐式 namespace 化。 |

### 5.2 配置契约测试（A-02）

除最后两组外，以下测试均放 `peri-middlewares/src/mcp/config_test.rs`；每个都可用 `cargo test -p peri-middlewares --lib -- <函数名>` 运行。新增文件场景使用 tempdir/NamedTempFile 和显式 global_path，禁止环境变量 HOME 切换、网络或子进程。

| 测试函数名 | 验收 / 可观察断言 |
| --- | --- |
| `test_system_mcp_project_rejects_tools_without_true` | 契约 1：项目四个非法组合返回 McpConfigError::ParseError；path 是 fixture 文件；source contains `system_mcp_tools requires system_mcp = true`。 |
| `test_system_mcp_global_rejects_invalid_maps` | 契约 1：nested/top-level 分别表驱动，均返回 ParseError 而非 Ok(empty)；双 map 时非法备用 map 也拒绝；路径和错误正文保留。 |
| `test_system_mcp_merged_errors_are_not_empty_success` | 契约 1：全局/项目/插件各放非法配置，在严格 full_with_paths 上均 Err；非法低优先级配置即使被有效项目同名覆盖也拒绝。 |
| `test_system_mcp_typed_validation_includes_disabled` | typed 构造非法 config，包括 disabled=true，validate_config 返回 InvalidServer；断言 server_name、具体 source 变体及完整固定 Display。 |
| `test_system_mcp_empty_tools_survive_config_pipeline` | 契约 4 配置部分：true + [] 经加载、三层合并、展开、禁用状态写回再载入仍 Some(empty)；不产生工具名，不推导 ready。 |
| `test_system_mcp_tools_survive_expansion_and_namespace` | 契约 3 配置部分：插件 server key 成为 plugin:p:s，工具 Vec 与输入逐项完全相等；global/project 同名覆盖为整条替换，source 保留；不是拼接数组。 |
| `test_system_mcp_dedup_preserves_required_namespaces` | 相同 command/args/env 的 System server 不因插件内容去重消失；普通 MCP 既有去重仍有效；变更 system/tools 字段改变 hash。 |
| `test_system_mcp_disabled_write_rejects_invalid_input` | 项目/nested/top-level 及 disabled true/false 矩阵：返回 ParseError，前后文件 bytes 完全相等，未触发原子替换。 |
| `test_system_mcp_remove_rejects_invalid_input` | 三种位置中目标或其他 server 非法均返回 ParseError，文件不变；删除非法目标不作为例外；合法删除仍成功。 |
| `test_system_mcp_write_preserves_remaining_tools` | 删除普通 server / 切换 disabled 后，剩余 System 数组顺序和值不变，[] 不被省略；全局其他 settings 字段仍存在。 |

`peri-middlewares/src/plugin/loader_test.rs`：

- `test_system_mcp_plugin_strict_sources_reject_invalid`：内联 manifest、文件引用 wrapped/flat、根 `.mcp.json` 各返回 LoaderError::McpConfigInvalid 或保留明确 manifest 解析 source 的错误；断言路径和固定规则正文；不部分接纳合法兄弟条目。
- `test_system_mcp_plugin_invalid_manifest_has_no_fallback`：非法现存 manifest 不触发 synthetic overwrite、不从根配置兜底；原文件 bytes 不变。
- `test_system_mcp_plugin_empty_manifest_map_has_no_fallback`：显式空 map 不从根加载额外服务器；无声明才允许根回退。
- `test_system_mcp_plugin_strict_error_reaches_merge`：启用插件非法配置到 full_with_paths 返回 PluginLoadError，source chain 保留规则正文；不是空 plugins 的成功结果。（该函数可放 config_test.rs 以访问私有路径 seam；最终归 A-02 单一 owner。）

`peri-middlewares/src/mcp/transport_test.rs`：`test_system_mcp_transport_rejects_invalid_typed_config`，断言 InvalidSystemConfig 的内层变体，证明公开 struct 构造不能绕过 transport 校验。

`peri-middlewares/src/mcp/initialize_test.rs`：`test_system_mcp_config_error_never_publishes_ready`，使用非法临时项目配置调用实际 run_initialize；watch 和 pool 状态为 Failed 且 message contains 固定规则正文，pool 未标 initialized，未出现 Ready、未开始 transport。不能以只测 parser 代替这条错误到状态 seam。

### 5.3 动态与跨 sub-plan 验收

- A-03 `test_dynamic_mcp_rejects_system_mcp_fields` 位于 `dynamic/tool_test.rs`：每个新 key/alias 都在 canonical bind 前拒绝，Err 不是成功空请求；deployment load 调用次数为 0。不要求未知字段错误与静态组合错误同变体。
- B/C 必须另有真实 seam 测试：`system_mcp=true, system_mcp_tools=[]` 时协议 ready 前 1R 不放行；ready 后 direct 列表中没有由此配置新增的 bridge。**A 只证明数组保真，不能证明 ready 或零注入。** 测试命名/文件由 B/C 冻结，A 不虚构它们已存在。
- 后续顺序：A 的 focused tests → B/C 的 transport/1R/工具视图 tests → workspace all-targets 检查和文档差异审阅。现有测试命令按 CLAUDE 路由；本轮没有执行任何 Cargo 命令，所有结果仍待实施验证。

## 6. 风险与未知

1. **timeout 未冻结**：设计只说配置的 timeout，没有定义静态 key、类型、单位、默认值或每 server/总等待范围。动态 `timeoutMs`/30,000ms 是独立 DTO 的事实，不能直接冒充静态设计。B 必须先决策；若新增字段仍落 plugin.rs，由 A owner 一次合并，更新 literal 清单。
2. **disabled + system=true**：A 验证规则不禁止该组合，设计未明示禁用与启动依赖谁优先；B 必须明确准入行为，不能在 A 默默改成普通 MCP。非法 tools 组合即便 disabled 也始终拒绝。
3. **公开 API 兼容性**：load_merged_config 改 Result 是有意的源码兼容性变化；仓库内未发现外部调用点，不等于不存在仓库外消费者。不得借保兼容继续提供 fail-open 返回；如版本政策要求迁移窗口，由父计划明确版本策略，不由 A 添 feature gate。
4. **serde helper 漂移**：必须逐字段保留本节列出的旧属性，并有旧 JSON/protocolVersion/source 回归。Option 本身默认接受 null，因此拒绝显式 null 必须真正实现 deserialize_with，不能仅写文档。
5. **插件宽容 API**：严格 MCP 入口与展示聚合必须共享解析器，不能共享吞错结果。展示路径保留安全诊断；任何将聚合 all_mcp_servers 转作 runtime 输入的新调用都是集成阻塞。未实跑插件安装、迁移、同步或 UI；这里只静态核实了路径。
6. **synthetic fallback 与外部文件**：仅缺文件允许生成、生成后严格解析；不修复非法 manifest。同步允许落盘非法字节但使用时拒绝。若父计划要求“同步/安装写磁盘时也原子拒绝所有非法输入”，那是更强的写入产品契约，需另确认范围，不能声称本计划覆盖整个文件同步事务。
7. **去重与 namespace 耦合**：普通 MCP 保留既有去重；System MCP 禁止被跨 namespace 内容去重删除。现有 hash 未包含 URL/headers 是既有问题，本计划不扩展为通用去重重构，也不调整凭据共享策略；C 必须知道 System 两个配置可同时留下。
8. **实施任务原子性**：字段加到 public struct 后全工作区中间态会暂时不编译，A-01 只运行契约 crate 检查，A-02 负责全部已检索 literals/调用者的闭环。不能把 B/C 同时修改 initialize 的冲突误报成编译事实。
9. **运行时证据缺口**：本轮只使用 Read/Grep/Glob 核实；未 build/test，未验证测试 fixture 的实际耗时/平台行为，也未确认仓库外消费者。最终不能将本文的命令列表写成验证通过记录。
10. **标准的准确引用**：API stability issue 不是现行编译期门禁，architecture-contracts 也没有字段新增 feature-gate 条款。确实适用的是类型归属、工具视图/生命周期/秘密保护等既有边界，不能凭推测扩大要求。

## 7. 非目标

- 不实现 5 个 MCP 实例真实迁移，不拆出独立 Workspace/Artifact/Web/Cron/LSP MCP server；不把 Filesystem、Terminal、Web、Cron、LSP、GitWatch、SkillTool 等从宿主下放。
- 不以本计划或绿色配置单测宣称验收契约 5、6 完成，更不宣称 v4 整体迁移完成；目标归属始终单列为尚未落地的目标。
- 不实现 B 的协议 initialize、能力/健康检查、超时、1R ready 屏障或关闭事务；只为配置失败接通 Failed 状态。
- 不实现 C 的所属 namespace 查找、schema 验证、effective tool name、bridge 构造、direct/deferred 分类及 RCRA 注入。
- 不改变 DynamicMCP 的 DTO、审批、session/incarnation、projection lease、加载/卸载 lifecycle；只保护它拒绝静态 System key 的现有边界。
- 不改变 protocolVersion 策略，不固定所有连接握手版本，不新增宿主 CLI 暴露入口。
- 不实施 API stability feature gate，不改插件启用优先级，不增加 MCP 热重载，不重构通用同步/安装事务。
- 本轮不写生产代码、测试或其他文档，不运行 cargo build/cargo test，不创建提交。
