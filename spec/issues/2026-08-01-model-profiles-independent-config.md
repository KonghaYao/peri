# Model Profile 独立配置与 Model Panel 重构

**状态**：Approved for planning
**优先级**：高
**类型**：功能 + 配置重构
**创建日期**：2026-08-01
**相关历史**：`2026-07-31-extract-peri-model-protocol-crate.md`

## 目标

将模型档位（Profile）从"共享一套 thinking/context 配置"改为**每档独立**：

- `fable` / `opus` / `sonnet` / `haiku` 四个固定 Profile，各自独立持有 `provider`、`model`、`effort`、`max_tokens`、`context_1m`；
- 切换 Profile 时整组配置生效；
- **Profile 是请求参数的唯一事实源**：废弃全局 `AppConfig.thinking`、`ProviderConfig.thinking`、`AppConfig.active_provider_id`、`AppConfig.context_1m`；
- 在 `opus` 之上新增 `fable` 档位（复用 `opus` 档位模型映射为空时的回退值）；
- 重构 Model Panel 为左右分栏：左侧 Profile 列表，右侧单行 K/V 编辑。

## 非目标

- 不修改 Provider 连接管理（apiKey/baseUrl 仍存于全局 `providers` 列表）；
- 不引入 Profile 增删改名的能力；
- 不做模型能力兼容性检查（`effort`/`max_tokens`/`context_1m` 不校验目标模型是否支持）；
- 不修改 `peri-model` 协议 crate 的请求 DTO 结构。

## 配置模型

### 新增

```rust
/// 单个 Profile 的独立配置（请求参数唯一事实源）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    /// 引用 providers[].id；空字符串表示未绑定 provider
    #[serde(default)]
    pub provider: String,
    /// 手动选择/输入的模型名；None 时回退到 provider.models 同档位映射
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// "low" | "medium" | "high" | "xhigh" | "max"，默认 "xhigh"
    #[serde(default = "default_profile_effort")]
    pub effort: String,
    /// 最大输出 token 数，默认 32000
    #[serde(default = "default_profile_max_tokens")]
    pub max_tokens: u32,
    /// 是否启用 1M 上下文，默认 false
    #[serde(default)]
    pub context_1m: bool,
}
```

`AppConfig` 新增固定四字段（不可增删改名的 map 型结构，直接作为 struct 字段）：

```rust
#[serde(default)]
pub profiles: Profiles, // { fable, opus, sonnet, haiku: ProfileConfig }
```

### 废弃（serde 兼容：旧字段由 `extra` 吸收，不回写）

| 字段 | 替代 |
|---|---|
| `AppConfig.thinking` | `profiles.*.effort` / `profiles.*.max_tokens` |
| `ProviderConfig.thinking` | 同上（Profile 唯一事实源） |
| `AppConfig.active_provider_id` | `profiles[active_alias].provider` |
| `AppConfig.context_1m` | `profiles.*.context_1m` |

### 保留并扩展

- `active_alias`：值域扩展为 `fable/opus/sonnet/haiku`，默认 `opus`；请求时决定使用哪个 Profile。
- `ProviderModels`：新增 `fable: String`；`get_model("fable")` 返回 fable，若 fable 为空则回退 `opus` 档位值。`get_model` 对四档均大小写不敏感。
- `providers`：全局列表不变，仍保存连接信息；Profile 的 `provider` 引用其中的 `id`。

### 覆盖规则（全局 / 项目级）

沿用 `merge_overrides` 机制，但按 **Profile 整体替换**：

- 项目级配置存在某 Profile → 完整使用项目级 Profile（不再读取全局同档字段，不做字段级合并）；
- 项目级不存在某 Profile → 使用全局完整 Profile；
- 项目级不允许新增/删除 Profile（固定四档）。

### 默认值

| Profile | provider | model | effort | max_tokens | context_1m |
|---|---|---|---|---|---|
| fable | "" | None（→ models.fable → models.opus） | xhigh | 32000 | false |
| opus | "" | None（→ models.opus） | xhigh | 32000 | false |
| sonnet | "" | None（→ models.sonnet） | xhigh | 32000 | false |
| haiku | "" | None（→ models.haiku） | xhigh | 32000 | false |

## 模型解析优先级

1. `profiles[active_alias].model` 非空 → 使用 Profile 值；
2. 否则 `providers[id].models.get_model(active_alias)`（`fable` 档位为空时回退 `opus`）；
3. 否则使用 `active_alias` 本身（保持现有 fallback 语义）。

Provider 解析：`profiles[active_alias].provider` 非空 → 在 `providers` 中按 id 查找；为空 → 沿用现有"第一个可用 provider"fallback。

## TUI Model Panel 设计

左右分栏（Panel 宽度不变，内部切分）：

```
┌─ Model ─────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Profiles                         fable · anthropic                          │
│  ─────────────────────────        ─────────────────────────────────────────  │
│  ❯ fable · anthropic              Provider                        anthropic │
│    claude-opus-4-6                Model                       claude-opus-4-6│
│    xhigh · 200k                   Effort                              xhigh  │
│                                    Max tokens                          32000  │
│    opus · anthropic               1m enable                              off  │
│    claude-opus-4-6                                                          │
│    xhigh · 200k                                                            │
│                                                                              │
│    sonnet · openai                ←/→ change value     esc close            │
│    gpt-5.6-luna                                                            │
│    xhigh · 1m                                                              │
│                                                                              │
│    haiku · anthropic                                                          │
│    claude-haiku-4-5                                                          │
│    medium · 200k                                                             │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 左侧 Profile 列表

- 固定顺序 `fable → opus → sonnet → haiku`；
- active Profile：`●` + 高亮背景/边框（沿用现有 active 样式），其余 `○`；
- 每项三行摘要：
  1. `<alias> · <provider 显示名>`；
  2. 实际 model 显示名（含 model 名中的 `high` 等后缀，用 model accent 色）；
  3. `<effort> · <200k/1m>`（effort 值用独立的 effort 色，200k/1m 用 token/context 标识色）；
- `↑/↓` 切换选中 Profile；`←` 或 `Enter` 进入右侧编辑焦点（若有）；
- 切换选中 Profile 即写 `active_alias` 并持久化。

### 右侧 K/V 编辑

- 每字段单行：`key` 左对齐、`value` 右对齐，无 `[]` 或其他包围符号；
- 字段固定：`Provider` / `Model` / `Effort` / `Max tokens` / `1m enable`；
- `↑/↓` 在字段间移动焦点；`←/→` 切换当前字段可选值；
- **切换值立即写入内存并持久化**（无需 Enter/Save；写入失败保留内存修改并显示错误提示）；
- `Provider` 切换联动：优先选择目标 provider 下同档位 Model；无同档位时选择该 provider 的默认 Model；
- `Model` 编辑允许选择该 provider 下任意模型（不按档位过滤，不做能力兼容性检查）；
- 手动选择的 model 写入 `profiles[alias].model`，不覆盖 `ProviderModels` 映射；
- `Effort` 沿用 5 档循环 `low → medium → high → xhigh → max`；
- `Max tokens` 沿用现有 5 档预设循环 `4096/8192/16000/32000/64000`（默认 32000）；
- `1m enable` 布尔切换；
- 右侧显示名规则：`gpt-5.6-luna` 的显示改为 `gpt-5.6-luna high`，其中模型名内的 `high` 使用 model accent 色；摘要中 effort 的 `high` 使用 effort 色，两者颜色语义不同。

## 请求链路影响点

| 文件 | 改动 |
|---|---|
| `peri-acp/src/provider/config.rs` | 新增 `ProfileConfig`/`Profiles`；`ProviderModels` 加 `fable` 与回退；废弃字段移除；`merge_overrides` 按 Profile 整体替换 |
| `peri-acp/src/provider/mod.rs` | `into_model`/LlmProvider 构造改为从 active Profile 读取 provider/model/effort/max_tokens/context_1m |
| `peri-acp/src/session/state_builders.rs` | SessionConfig 快照改为从 active Profile 读取 effort/max_tokens，不再用 `thinking`/`active_provider_id` |
| `peri-acp/src/session/mod.rs` | `SessionInfo` 的 provider_id/model_alias 取自 active Profile |
| `peri-acp/src/session/executor.rs`、`agent/builder.rs` | `context_1m` 从 active Profile 读取 |
| `peri-tui/src/kit/service_snapshot.rs` | 派生 (provider_type, alias, model_name) 改为 Profile 优先 + ProviderModels 回退 |
| `peri-tui/src/acp_server/requests.rs`、`acp_stdio/session/config.rs` | `update_config` 处理 `active_alias`、profiles 字段；移除 `active_provider_id`/`context_1m`/`thinking` 顶层处理 |
| `peri-tui/src/kit/panels/model.rs` | 重构为左右分栏 Profile 面板（或新增 `profile.rs` 替换注册） |
| `peri-tui/src/kit/panels/agent.rs`、`config.rs`、`login.rs`、`submit_consumer.rs`、`entry.rs`、`setup_wizard` | 从 `active_provider_id`/`thinking` 迁移到 active Profile |
| `peri-theme` | 如缺少 model accent / effort 独立色，在语义 token 中补充 |

## 旧配置迁移

- 旧 `thinking.effort` / `thinking.max_tokens` / `context_1m` → 作为四档初始值写入 profiles（fable 同值）；
- 旧 `active_provider_id` → 作为四档初始 `provider`；
- `fable` 初始 model：走 `ProviderModels.fable`，为空回退 `models.opus`；
- 旧字段不再序列化（由 `extra` 吸收），不主动写回。

## 测试计划

- `peri-acp` 单测：
  - `ProfileConfig` serde 默认值（effort=xhigh、max_tokens=32000、context_1m=false、model=None）；
  - `ProviderModels.get_model` 四档 + 大小写 + `fable` 回退 `opus`；
  - model 解析优先级（Profile 值 > models 映射 > alias fallback）；
  - `merge_overrides`：项目级 Profile 整体替换、未定义 Profile 保留全局、不允许新增/删除；
  - `into_model` 请求参数全部来自 active Profile（effort/max_tokens/context_1m）。
- `peri-tui` 单测：
  - 左列固定顺序与 active 标记；
  - 右侧 `←/→` 立即持久化；
  - Provider 切换联动 Model（同档位优先，无则默认）；
  - 三行摘要与两处 `high` 的不同颜色 span。
- E2E：`e2e/tests/panels/model-switch.test.ts` 更新为新交互（Profile 切换 + K/V 编辑）。
- 文档：更新 `TUI-PAGE.md`（已写入初始版）与 `spec/global/domains/tui/tui-panels.md`、`tui-index.md` 的 Model 面板描述。
