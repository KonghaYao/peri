# Model Profile 独立配置与 Model Panel 重构 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将模型档位从共享配置改为 `fable/opus/sonnet/haiku` 四个独立 Profile（各自持有 provider/model/effort/max_tokens/context_1m），并把 Model Panel 重构为左右分栏 Profile 编辑器。

**Architecture:** Profile 成为请求参数唯一事实源。`AppConfig` 新增固定四档 `profiles`，废弃全局 `thinking`/`active_provider_id`/`context_1m`；`LlmProvider` 改为携带扁平 `effort/max_tokens/context_1m` 字段并在 `into_model()` 中以 `budget_tokens = max_tokens` 构造 Anthropic extended thinking。TUI Model Panel 改为左 Profile 列表 + 右 K/V 编辑，切换值立即持久化。

**Tech Stack:** Rust workspace（peri-acp / peri-tui / peri-theme）、ratatui-kit `#[component]`、serde、tokio。

**Spec:** `spec/issues/2026-08-01-model-profiles-independent-config.md`

---

## 阶段 1：配置模型（`peri-acp/src/provider/config.rs`）

### Task 1: `ProviderModels` 增加 `fable` 档位与回退

**Files:**
- Modify: `peri-acp/src/provider/config.rs:19-40`
- Test: `peri-acp/src/provider/config_test.rs`

- [ ] **Step 1: 写失败测试**

在 `peri-acp/src/provider/config_test.rs` 增加：

```rust
#[test]
fn provider_models_fable_tier_and_fallback() {
    let m = ProviderModels {
        opus: "claude-opus-4-6".into(),
        sonnet: "claude-sonnet-4-6".into(),
        haiku: "claude-haiku-4-5".into(),
        fable: String::new(),
    };
    // fable 档位为空 → 回退 opus
    assert_eq!(m.get_model("fable"), Some("claude-opus-4-6"));
    assert_eq!(m.get_model("FABLE"), Some("claude-opus-4-6"));
    let m2 = ProviderModels {
        fable: "claude-fable-1-0".into(),
        ..m
    };
    assert_eq!(m2.get_model("fable"), Some("claude-fable-1-0"));
    assert_eq!(m2.get_model("opus"), Some("claude-opus-4-6"));
    assert_eq!(m2.get_model("sonnet"), Some("claude-sonnet-4-6"));
    assert_eq!(m2.get_model("haiku"), Some("claude-haiku-4-5"));
    assert_eq!(m2.get_model("turbo"), None);
}
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test -p peri-acp --lib provider_models_fable_tier_and_fallback
```
Expected: FAIL（`ProviderModels` 尚无 `fable` 字段）。

- [ ] **Step 3: 实现**

```rust
pub struct ProviderModels {
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
    /// fable 档位模型名；为空时回退到 opus 档位
    #[serde(default)]
    pub fable: String,
}

impl ProviderModels {
    pub fn get_model(&self, alias: &str) -> Option<&str> {
        match alias.to_lowercase().as_str() {
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            "fable" => Some(if self.fable.is_empty() { &self.opus } else { &self.fable }),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: 运行确认通过**

```bash
cargo test -p peri-acp --lib provider_models_fable_tier_and_fallback
```
Expected: PASS

- [ ] **Step 5: 修复既有测试结构体字面量**

`ProviderModels { opus, sonnet, haiku }` 结构体字面量现在缺 `fable` 字段，编译器报错位置即需补 `fable: String::new()` 或 `..Default::default()`：
- `peri-tui/src/config/types_test.rs`（多个字面量）
- `peri-acp/src/provider/config_test.rs`（若有用到）

```bash
cargo build -p peri-acp -p peri-tui
```

- [ ] **Step 6: Commit**

```bash
git add peri-acp/src/provider/config.rs peri-acp/src/provider/config_test.rs peri-tui/src/config/types_test.rs
git commit -m "feat: ProviderModels 增加 fable 档位与 opus 回退"
```

### Task 2: 新增 `ProfileConfig` / `Profiles`

**Files:**
- Modify: `peri-acp/src/provider/config.rs`
- Test: `peri-acp/src/provider/config_test.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn profile_config_defaults() {
    let p = ProfileConfig::default();
    assert_eq!(p.provider, "");
    assert_eq!(p.model, None);
    assert_eq!(p.effort, "xhigh");
    assert_eq!(p.max_tokens, 32000);
    assert!(!p.context_1m);
}

#[test]
fn profiles_serde_roundtrip_four_tiers() {
    let json = r#"{
        "fable":   { "provider": "a", "effort": "max",   "max_tokens": 64000, "context_1m": true },
        "opus":    { "provider": "a" },
        "sonnet":  {},
        "haiku":   { "provider": "b", "model": "gpt-5.6-luna", "effort": "medium", "max_tokens": 16000, "context_1m": false }
    }"#;
    let profiles: Profiles = serde_json::from_str(json).unwrap();
    assert_eq!(profiles.fable.provider, "a");
    assert_eq!(profiles.fable.effort, "max");
    assert!(profiles.fable.context_1m);
    assert_eq!(profiles.opus.effort, "xhigh"); // 缺省字段用默认
    assert_eq!(profiles.opus.max_tokens, 32000);
    assert_eq!(profiles.haiku.model.as_deref(), Some("gpt-5.6-luna"));
    // 固定四档顺序（序列化顺序）
    let back = serde_json::to_value(&profiles).unwrap();
    assert!(back.get("fable").is_some() && back.get("opus").is_some()
        && back.get("sonnet").is_some() && back.get("haiku").is_some());
}
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test -p peri-acp --lib profile_config_defaults profiles_serde_roundtrip_four_tiers
```
Expected: FAIL（类型不存在）。

- [ ] **Step 3: 实现**

```rust
fn default_profile_effort() -> String { "xhigh".to_string() }
fn default_profile_max_tokens() -> u32 { 32000 }

/// 单个 Profile 的独立配置（请求参数唯一事实源）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_profile_effort")]
    pub effort: String,
    #[serde(default = "default_profile_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub context_1m: bool,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: None,
            effort: default_profile_effort(),
            max_tokens: default_profile_max_tokens(),
            context_1m: false,
        }
    }
}

/// 固定四档 Profile（不可增删改名）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profiles {
    #[serde(default)]
    pub fable: ProfileConfig,
    #[serde(default)]
    pub opus: ProfileConfig,
    #[serde(default)]
    pub sonnet: ProfileConfig,
    #[serde(default)]
    pub haiku: ProfileConfig,
}

impl Profiles {
    pub const ALL: [&'static str; 4] = ["fable", "opus", "sonnet", "haiku"];

    pub fn get(&self, alias: &str) -> Option<&ProfileConfig> {
        match alias.to_lowercase().as_str() {
            "fable" => Some(&self.fable),
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, alias: &str) -> Option<&mut ProfileConfig> {
        match alias.to_lowercase().as_str() {
            "fable" => Some(&mut self.fable),
            "opus" => Some(&mut self.opus),
            "sonnet" => Some(&mut self.sonnet),
            "haiku" => Some(&mut self.haiku),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: 运行确认通过**

```bash
cargo test -p peri-acp --lib profile_config_defaults profiles_serde_roundtrip_four_tiers
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add peri-acp/src/provider/config.rs peri-acp/src/provider/config_test.rs
git commit -m "feat: 新增 ProfileConfig/Profiles 四档独立配置"
```

### Task 3: `AppConfig` 字段变更（废弃 thinking / active_provider_id / context_1m）

**Files:**
- Modify: `peri-acp/src/provider/config.rs:88-138`、`:197-218`、`:144-195`
- Test: `peri-acp/src/provider/config_test.rs`、`peri-tui/src/config/types_test.rs`

- [ ] **Step 1: 改 `AppConfig` 定义与 `Default`**

```rust
pub struct AppConfig {
    #[serde(default = "default_alias")]
    pub active_alias: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 四档 Profile（唯一事实源）
    #[serde(default)]
    pub profiles: Profiles,
    #[serde(default, alias = "skillsDir")]
    pub skills_dir: Option<String>,
    // ── 其余字段不变（env/compact/language/persona/tone/claude_md_excludes/proactiveness/show_cache_warning/betas/extra）──
}
```

`Default` 中：移除 `active_provider_id` / `thinking` / `context_1m`，新增 `profiles: Profiles::default()`。

- [ ] **Step 2: 改 `merge_overrides`（按 Profile 整体替换）**

```rust
impl AppConfig {
    pub fn merge_overrides(&mut self, workspace: AppConfig) {
        if !workspace.providers.is_empty() {
            self.providers = workspace.providers;
        }
        if !workspace.active_alias.is_empty() {
            self.active_alias = workspace.active_alias;
        }
        // Profile：项目级存在某档位 → 整体替换；不存在 → 保留全局（不做字段级合并）
        for alias in Profiles::ALL {
            if let Some(ws) = workspace.profiles.get(alias) {
                if let Some(global) = self.profiles.get_mut(alias) {
                    *global = ws.clone();
                }
            }
        }
        // 其余 Option<bool>/Option<T> 字段覆盖逻辑保持不变；删除 thinking/context_1m 分支
        self.show_cache_warning = workspace.show_cache_warning;
        self.extra.extend(workspace.extra);
    }
}
```

- [ ] **Step 3: 更新测试**

`peri-acp/src/provider/config_test.rs` 与 `peri-tui/src/config/types_test.rs` 中引用 `thinking`/`active_provider_id`/`context_1m` 的断言全部改为新结构。新增：

```rust
#[test]
fn merge_overrides_profile_whole_replacement() {
    let mut global = AppConfig {
        profiles: Profiles {
            opus: ProfileConfig { effort: "high".into(), max_tokens: 32000, ..Default::default() },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ws = AppConfig::default();
    // 项目级只覆盖 opus
    ws.profiles.get_mut("opus").unwrap().effort = "max".into();
    ws.profiles.get_mut("opus").unwrap().max_tokens = 64000;
    global.merge_overrides(ws);
    assert_eq!(global.profiles.opus.effort, "max");
    assert_eq!(global.profiles.opus.max_tokens, 64000);
    // 项目级未定义 fable → 保留全局
    assert_eq!(global.profiles.fable.effort, "xhigh");
}

#[test]
fn serde_deprecated_fields_absorbed_into_extra() {
    let json = r#"{"active_alias":"opus","active_provider_id":"a","thinking":{"enabled":true,"effort":"high"},"context_1m":true,"providers":[]}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.active_alias, "opus");
    // 废弃字段进入 extra，不回写
    assert!(cfg.extra.contains_key("active_provider_id"));
    assert!(cfg.extra.contains_key("thinking"));
    assert!(cfg.extra.contains_key("context_1m"));
}
```

- [ ] **Step 4: 运行**

```bash
cargo test -p peri-acp --lib config
cargo test -p peri-tui --lib config
```
Expected: PASS（`merge_overrides_profile_whole_replacement`、`serde_deprecated_fields_absorbed_into_extra` 通过；其余引用旧字段的测试已同步更新）。

- [ ] **Step 5: Commit**

```bash
git add peri-acp/src/provider/config.rs peri-acp/src/provider/config_test.rs peri-tui/src/config/types_test.rs
git commit -m "feat: AppConfig 废弃 thinking/active_provider_id/context_1m，新增 profiles 并按档位整体覆盖"
```

---

## 阶段 2：LlmProvider 重构（`peri-acp/src/provider/mod.rs`）

### Task 4: `LlmProvider` 携带扁平 effort/max_tokens/context_1m

**Files:**
- Modify: `peri-acp/src/provider/mod.rs:14-29`、`:226-279`
- Test: `peri-acp/src/provider/provider_test.rs`

- [ ] **Step 1: 改变体字段**

```rust
#[derive(Clone)]
pub enum LlmProvider {
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
        /// 思考强度 "low".."max"；None 表示不启用 extended thinking
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
    },
}
```

新增访问器：

```rust
pub fn context_1m(&self) -> bool {
    match self {
        Self::OpenAi { context_1m, .. } | Self::Anthropic { context_1m, .. } => *context_1m,
    }
}

/// 思考强度稳定标识，用于 fingerprint；None 时返回空字符串
pub fn effort_key(&self) -> String {
    match self {
        Self::OpenAi { effort, .. } | Self::Anthropic { effort, .. } => {
            effort.as_ref().map(|e| format!(":effort={e}")).unwrap_or_default()
        }
    }
}
```

- [ ] **Step 2: 更新 `into_model()`**

```rust
Self::OpenAi { api_key, base_url, model, effort, max_tokens, .. } => {
    let endpoint = parse_endpoint(&base_url, "https://api.openai.com/v1", "openai base_url");
    let mut config = OpenAiConfig::new(endpoint, api_key, model);
    if let Some(e) = effort.as_ref() {
        config = config.with_reasoning_effort(e);
        config = config.with_thinking_enabled(true);
    }
    config = config.with_max_tokens(max_tokens);
    Box::new(OpenAiModel::new(config))
}
Self::Anthropic { api_key, model, base_url, effort, max_tokens, .. } => {
    let endpoint = match base_url {
        Some(url) => parse_endpoint(url, "https://api.anthropic.com", "anthropic base_url"),
        None => Url::parse("https://api.anthropic.com").expect("静态默认 endpoint"),
    };
    let mut config = AnthropicConfig::new(endpoint, api_key, model);
    if let Some(e) = effort.as_ref() {
        // budget_tokens = max_tokens（Profile 唯一事实源，不新增字段）
        config = config.with_extended_thinking(max_tokens, e);
    }
    config = config.with_max_tokens(max_tokens);
    Box::new(AnthropicModel::new(config))
}
```

删除 `thinking_key()`（改为 `effort_key()`）；`context_window()` 不变（1M 由 `context_1m()` 标志在调用侧覆盖）。

- [ ] **Step 3: 更新 `from_env()` 变体构造**

`from_env` 中 `thinking: None` → `effort: None, max_tokens: 32000, context_1m: false`（共 4 处）。

- [ ] **Step 4: 编译 + 修复测试**

```bash
cargo build -p peri-acp
```
修复 `provider_test.rs`：`think(...)` helper 与字面量改为扁平字段。`thinking_key` 断言改为 `effort_key`；`into_model` 断言改读 `max_tokens`。

- [ ] **Step 5: Commit**

```bash
git add peri-acp/src/provider/mod.rs peri-acp/src/provider/provider_test.rs
git commit -m "feat: LlmProvider 改用扁平 effort/max_tokens/context_1m 字段"
```

### Task 5: `from_config` / `from_config_for_alias` 从 Profile 读取

**Files:**
- Modify: `peri-acp/src/provider/mod.rs:90-186`
- Test: `peri-acp/src/provider/provider_test.rs`

- [ ] **Step 1: 新增内部解析函数**

```rust
/// 从 AppConfig 解析 active profile → (provider, profile)
fn resolve_profile<'a>(app: &'a config::AppConfig, alias: &str) -> Option<(&'a ProviderConfig, &'a config::ProfileConfig)> {
    let profile = app.profiles.get(alias)?;
    let provider = if profile.provider.is_empty() {
        app.providers.first()
    } else {
        app.providers.iter().find(|p| p.id == profile.provider)
    }?;
    Some((provider, profile))
}

/// 解析最终 model 名：Profile.model > ProviderModels 同档位(fable 空回退 opus) > 厂商默认
fn resolve_model_name(provider: &ProviderConfig, alias: &str, profile: &config::ProfileConfig) -> String {
    if let Some(m) = profile.model.as_ref().filter(|m| !m.is_empty()) {
        return m.clone();
    }
    provider.models.get_model(alias)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match provider.provider_type.as_str() {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            _ => "gpt-4o".to_string(),
        })
}
```

- [ ] **Step 2: 重写 `from_config` / `from_config_for_alias`**

`from_config(cfg)` 以 `app.active_alias` 调用 `from_config_for_alias(cfg, alias)`。`from_config_for_alias`：

```rust
pub fn from_config_for_alias(cfg: &config::PeriConfig, alias: &str) -> Option<Self> {
    let app = &cfg.config;
    let (provider, profile) = resolve_profile(app, alias)?;
    if provider.api_key.is_empty() {
        return None;
    }
    let model = resolve_model_name(provider, alias, profile);
    let effort = Some(profile.effort.clone());
    let max_tokens = profile.max_tokens;
    let context_1m = profile.context_1m;
    match provider.provider_type.as_str() {
        "anthropic" => Some(Self::Anthropic {
            api_key: provider.api_key.clone(),
            model,
            base_url: if provider.base_url.is_empty() { None } else { Some(provider.base_url.clone()) },
            effort,
            max_tokens,
            context_1m,
        }),
        _ => Some(Self::OpenAi {
            api_key: provider.api_key.clone(),
            base_url: if provider.base_url.is_empty() {
                "https://api.openai.com/v1".to_string()
            } else {
                provider.base_url.clone()
            },
            model,
            effort,
            max_tokens,
            context_1m,
        }),
    }
}
```

- [ ] **Step 3: 更新测试**

`provider_test.rs`：`from_config` 构造含 `profiles`；断言 profile 的 effort/max_tokens/context_1m 生效；`from_config_for_alias("fable")` 在 `models.fable` 空时回退 `models.opus`。新增：

```rust
#[test]
fn from_config_reads_active_profile() {
    let cfg = PeriConfig {
        config: AppConfig {
            active_alias: "opus".into(),
            profiles: Profiles {
                opus: ProfileConfig {
                    provider: "p1".into(),
                    effort: "max".into(),
                    max_tokens: 64000,
                    context_1m: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            providers: vec![ProviderConfig { id: "p1".into(), provider_type: "openai".into(), api_key: "k".into(), models: ProviderModels { opus: "gpt-x".into(), ..Default::default() }, ..Default::default() }],
            ..Default::default()
        },
    };
    let p = LlmProvider::from_config(&cfg).unwrap();
    assert_eq!(p.model_name(), "gpt-x");
    assert!(p.context_1m());
    assert_eq!(p.effort_key(), ":effort=max");
    let m = p.into_model();
    // 具体 max_tokens 通过模型构造参数断言（保留 openai 适配器现有断言方式）
    let _ = m;
}
```

- [ ] **Step 4: 运行**

```bash
cargo test -p peri-acp --lib provider
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add peri-acp/src/provider/mod.rs peri-acp/src/provider/provider_test.rs
git commit -m "feat: LlmProvider 从 active profile 解析 provider/model/effort/max_tokens/context_1m"
```

---

## 阶段 3：Session 层

### Task 6: `state_builders.rs` 从 Profile 读取

**Files:**
- Modify: `peri-acp/src/session/state_builders.rs`
- Test: `peri-acp/src/session/mod_test.rs`

- [ ] **Step 1: 改 `apply_thinking_effort` → `apply_profile_effort`**

```rust
/// 将 effort 写入 active profile（Profile 唯一事实源）
pub fn apply_profile_effort(peri_config: &RwLock<PeriConfig>, effort: &str) {
    let mut cfg = peri_config.write();
    let alias = cfg.config.active_alias.clone();
    if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
        profile.effort = effort.to_string();
    }
}
```

保留 `pub fn apply_thinking_effort(...)` 作为薄包装（调用 `apply_profile_effort`）以兼容 ACP 旧协议消息，标注 `#[deprecated]`。

- [ ] **Step 2: 改 `build_config_options`**

- Model 选项：遍历 `Profiles::ALL`（四档），从 `resolve_model_name` 语义取各档模型名：

```rust
let active_provider = peri_config.config.providers.iter().find(|p| {
    let alias = &peri_config.config.active_alias;
    peri_config.config.profiles.get(alias).map(|pf| !pf.provider.is_empty() && p.id == pf.provider).unwrap_or(true)
});
let mut model_options = Vec::new();
for alias in Profiles::ALL {
    let profile = peri_config.config.profiles.get(alias).unwrap();
    let model_name = active_provider
        .and_then(|p| p.models.get_model(alias))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| profile.model.clone())
        .unwrap_or_else(|| alias.to_string());
    model_options.push(SessionConfigSelectOption::new(
        SessionConfigValueId::new(alias.to_string()),
        format!("{alias} ({model_name})"),
    ));
}
```

- Thinking effort 值：`peri_config.config.profiles.get(&peri_config.config.active_alias).map(|p| p.effort.as_str()).unwrap_or("xhigh")`。
- 移除 `ThinkingConfig` import 依赖（该类型即将废弃）。

- [ ] **Step 3: 运行**

```bash
cargo test -p peri-acp --lib session
```
Expected: PASS（`mod_test.rs` 中 `active_provider_id`/`thinking` 相关断言同步更新为 profile 结构）。

- [ ] **Step 4: Commit**

```bash
git add peri-acp/src/session/state_builders.rs peri-acp/src/session/mod_test.rs
git commit -m "feat: session config options 从 active profile 读取 effort 与四档模型"
```

### Task 7: `AcpSession` / executor / builder 的 context_1m

**Files:**
- Modify: `peri-acp/src/session/mod.rs:209-212`、`peri-acp/src/session/executor.rs:428`、`peri-acp/src/agent/builder.rs:439`

- [ ] **Step 1: `AcpSession` 字段**

`provider_id` 改为 active profile 的 provider；移除 `thinking` 字段（无消费点，已确认）：

```rust
let active_alias = self.inner.peri_config.config.active_alias.clone();
let provider_id = self.inner.peri_config.config.profiles.get(&active_alias).map(|p| p.provider.clone()).unwrap_or_default();
// ...
model_alias: active_alias,
// 移除 thinking 字段及其在 AcpSession 结构体/构造点的声明
```

同步删除 `AcpSession` 结构体的 `thinking` 字段定义（`peri-acp/src/session/mod.rs` 结构体处）。

- [ ] **Step 2: executor.rs**

```rust
let context_1m = ctx.provider.context_1m();
```

（替换 `ctx.peri_config.config.context_1m.unwrap_or(false)`。）

- [ ] **Step 3: builder.rs**

```rust
let context_1m = ctx.provider.context_1m();
```

（替换 `peri_config.config.context_1m.unwrap_or(false)`；确认该作用域 `ctx.provider` 可访问——`LlmProvider` 已实现 `context_1m()`。）

- [ ] **Step 4: 编译**

```bash
cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add peri-acp/src/session/mod.rs peri-acp/src/session/executor.rs peri-acp/src/agent/builder.rs
git commit -m "feat: session/executor/builder 的 provider_id 与 context_1m 取自 active profile"
```

---

## 阶段 4：TUI 服务层

### Task 8: `service_snapshot.rs` 从 Profile 派生

**Files:**
- Modify: `peri-tui/src/kit/service_snapshot.rs:398-444`

- [ ] **Step 1: 改 `derive_provider_and_model`**

```rust
fn derive_provider_and_model(peri_config: &SharedPeriConfig) -> (String, String, String) {
    let cfg = peri_config.read();
    let active_alias = cfg.config.active_alias.clone();
    let profile = cfg.config.profiles.get(&active_alias);

    let provider = profile.and_then(|pf| {
        if pf.provider.is_empty() {
            cfg.config.providers.first()
        } else {
            cfg.config.providers.iter().find(|p| p.id == pf.provider)
        }
    });

    let provider_type = provider
        .map(|p| p.provider_type.clone())
        .unwrap_or_else(|| profile.map(|pf| pf.provider.clone()).unwrap_or_else(|| active_alias.clone()));

    let model_name = if let Some(m) = profile.and_then(|pf| pf.model.as_ref()).filter(|m| !m.is_empty()) {
        m.clone()
    } else {
        provider
            .and_then(|p| p.models.get_model(&active_alias))
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| active_alias.clone())
    };

    (provider_type, active_alias, model_name)
}
```

- [ ] **Step 2: 改 `derive_providers` 的 is_active**

```rust
let active_profile_provider = cfg.config.profiles.get(&cfg.config.active_alias).map(|p| p.provider.clone()).unwrap_or_default();
// ...
is_active: p.id == active_profile_provider,
```

- [ ] **Step 3: 运行**

```bash
cargo test -p peri-tui --lib service_snapshot
```
Expected: PASS（`service_snapshot_test.rs` 中 `active_provider_id` 断言改为 profile 结构）。

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/service_snapshot.rs peri-tui/src/kit/service_snapshot_test.rs
git commit -m "feat: service snapshot 从 active profile 派生 provider/model"
```

### Task 9: ACP `set_config_option` 与 `update_config`

**Files:**
- Modify: `peri-tui/src/acp_server/requests.rs:202-273`
- Modify: `peri-tui/src/acp_stdio/session/config.rs:20-145`
- Test: `peri-tui/src/acp_server/requests_test.rs`

- [ ] **Step 1: `requests.rs` `set_config_option`**

- `"model"`：保持不变（写 `active_alias`），但重建 provider 改用 `LlmProvider::from_config(&c)`（active profile 语义自动生效）。
- `"thinking_effort"`：改用 `apply_profile_effort(&cfg.peri_config, value)`（替代 `apply_thinking_effort`）；重建 provider 用 `LlmProvider::from_config(&c)`。
- `"context_1m"`：写入 active profile：

```rust
"context_1m" => {
    let enabled = value == "true" || value == "1";
    {
        let mut c = cfg.peri_config.write();
        let alias = c.config.active_alias.clone();
        if let Some(profile) = c.config.profiles.get_mut(&alias) {
            profile.context_1m = enabled;
        }
    }
    persist_config(cfg);
}
```

- [ ] **Step 2: `acp_stdio/session/config.rs`**

`set_config_option` 内 `"thinking_effort"` / `"context_1m"` 分支同样改为写 active profile；`handle_update_config` 中删除 `active_provider_id` 校验，改为校验 `profiles` 中各 profile 的 provider 均存在于 `providers`（不存在则报错 `invalid_request`）。

```rust
for alias in peri_tui::config::Profiles::ALL {
    let pid = new_cfg.config.profiles.get(alias).map(|p| p.provider.as_str()).unwrap_or("");
    if !pid.is_empty() && !new_cfg.config.providers.iter().any(|p| p.id == pid) {
        return Err(Error::invalid_request().data(format!("profile {alias}: provider '{pid}' not found")));
    }
}
```

- [ ] **Step 3: 更新 `requests_test.rs`**

`active_provider_id` 相关测试改为构造 `profiles`（如 `profiles.opus.provider = "a"`），断言切换 profile provider 后 `LlmProvider::from_config` 正确解析。

- [ ] **Step 4: 运行**

```bash
cargo test -p peri-tui --lib acp_server
cargo test -p peri-tui --lib acp_stdio
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/acp_server/requests.rs peri-tui/src/acp_stdio/session/config.rs peri-tui/src/acp_server/requests_test.rs
git commit -m "feat: ACP set_config_option/update_config 适配 active profile"
```

### Task 10: `submit_consumer.rs` CycleProvider → CycleAlias

**Files:**
- Modify: `peri-tui/src/kit/submit_consumer.rs:202-219`

- [ ] **Step 1: 改 `CycleProvider`**

`ViewActionRequest::CycleProvider` 保留事件名，语义改为循环 `active_alias` 四档：

```rust
ViewActionRequest::CycleProvider => {
    if let Some(cfg_handle) = PERI_CONFIG_HANDLE.get() {
        let cfg = cfg_handle.read();
        let aliases = ["fable", "opus", "sonnet", "haiku"];
        let current = &cfg.config.active_alias;
        let idx = aliases.iter().position(|a| *a == current).unwrap_or(0);
        let next = aliases[(idx + 1) % aliases.len()].to_string();
        drop(cfg);
        let client = acp_client.clone();
        let cfg_handle = cfg_handle.clone();
        tokio::spawn(async move {
            let mut new_cfg = cfg_handle.read().clone();
            new_cfg.config.active_alias = next;
            let _ = client.update_config(&new_cfg).await;
        });
    }
}
```

- [ ] **Step 2: 编译 + Commit**

```bash
cargo build -p peri-tui
git add peri-tui/src/kit/submit_consumer.rs
git commit -m "feat: CycleProvider 快捷键改为循环 active profile"
```

### Task 11: Login 面板激活语义

**Files:**
- Modify: `peri-tui/src/kit/panels/login.rs:261-300`

- [ ] **Step 1: Enter 激活改为写 active profile 的 provider**

```rust
KeyCode::Enter => {
    let sel = *cursor.read();
    let latest_providers = PROVIDER_LIST.state().read().clone();
    if let Some(p) = latest_providers.get(sel) {
        let provider_id = p.id.clone();
        let provider_type = p.provider_type.clone();
        if let Some(handle) = PERI_CONFIG_HANDLE.get() {
            let mut cfg = handle.write();
            let alias = cfg.config.active_alias.clone();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.provider = provider_id.clone();
            }
            let snap = cfg.clone();
            drop(cfg);
            // 联动 model：target provider 无同档位时保留原 model（不强制覆盖），
            // 但清空手动 model 以回退 ProviderModels 映射
            if let Some(active_prov) = snap.config.providers.iter().find(|p| p.id == provider_id) {
                let resolved_name = active_prov
                    .models
                    .get_model(&snap.config.active_alias)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| snap.config.active_alias.clone());
                let s_handle = SERVICE_SNAPSHOT.state();
                let mut svc_snap = s_handle.read().clone();
                svc_snap.provider_name = provider_type;
                svc_snap.model_name = resolved_name;
                *s_handle.write() = svc_snap;
            }
            crate::config::save(&snap).ok();
            // 推送 ACP 服务端
            if let Some(client) = ACP_CLIENT_HANDLE.get() {
                let _ = client.update_config(&snap);
            }
        }
        // PROVIDER_LIST is_active 更新逻辑保持不变，但比较对象改为 active profile 的 provider
    }
}
```

- [ ] **Step 2: 编译**

```bash
cargo build -p peri-tui
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/panels/login.rs
git commit -m "feat: Login 面板激活写 active profile 的 provider"
```

### Task 12: 其余 `active_provider_id` / `thinking` 引用清理

**Files:**
- Modify: `peri-tui/src/kit/entry.rs:474`（`is_active: p.id == cfg.config.active_provider_id` → active profile provider）
- Modify: `peri-tui/src/kit/panels/agent.rs:82-118`（provider 显示改为 active profile）
- Modify: `peri-tui/src/kit/panels/config.rs:252,362,578`（`context_1m` 行改为读写 active profile；`active_alias` 循环四档）
- Modify: `peri-tui/src/kit/acp_stdio/model.rs:17`（回写 active_alias 不变，但 provider 重建用 `from_config`）
- Modify: `peri-tui/src/app/setup_wizard/mod.rs:385-412`（向导初始化：移除 `active_provider_id`，写入 `profiles.*.provider`）
- Modify: `peri-tui/src/kit/panels/login.rs:893,910-914,972`（provider id 变更时同步 active profile 的 provider）

- [ ] **Step 1: 逐个文件替换**

替换模式统一为：读取 `cfg.config.profiles.get(&cfg.config.active_alias)` 的 `provider` 字段替代 `active_provider_id`。`config.rs` 面板的 `context_1m` 编辑改为：

```rust
let alias = cfg.config.active_alias.clone();
if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
    profile.context_1m = !profile.context_1m;
}
```

- [ ] **Step 2: 编译 + 全量单测**

```bash
cargo build --workspace
cargo test -p peri-tui --lib
cargo test -p peri-acp --lib
```
修复剩余引用旧字段的测试。Expected: 全绿。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src
git commit -m "refactor: 清理 active_provider_id/thinking 引用，统一走 active profile"
```

---

## 阶段 5：Model Panel 重构

### Task 13: `model.rs` 左右分栏 Profile 编辑器

**Files:**
- Rewrite: `peri-tui/src/kit/panels/model.rs`

- [ ] **Step 1: 保留的静态常量**

```rust
const PROFILE_KEYS: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];
const PROFILE_NAMES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];
const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const MAX_TOKEN_PRESETS: &[u32] = &[4096, 8192, 16000, 32000, 64000];
/// 右侧字段索引：Provider / Model / Effort / Max tokens / 1m enable
const FIELD_PROVIDER: usize = 0;
const FIELD_MODEL: usize = 1;
const FIELD_EFFORT: usize = 2;
const FIELD_MAX_TOKENS: usize = 3;
const FIELD_CONTEXT_1M: usize = 4;
const FIELD_COUNT: usize = 5;
```

- [ ] **Step 2: 组件状态与事件**

```rust
#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);          // 左侧 profile 光标
    let right_cursor = hooks.use_state(|| 0usize);     // 右侧字段光标
    let right_focus = hooks.use_state(|| false);       // 是否在右侧编辑焦点
    let render_version = hooks.use_state(|| 0u64);
    let snapshot = hooks.use_atom(&SERVICE_SNAPSHOT);
    let active_alias = snapshot.read().model_alias.clone();
    let _ = snapshot;
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else { return EventResult::Ignored };
            if key.kind != KeyEventKind::Press { return EventResult::Ignored };
            match key.code {
                KeyCode::Esc => {
                    if *right_focus.read() { *right_focus.write() = false; }
                }
                KeyCode::Up => {
                    if *right_focus.read() {
                        let mut c = right_cursor.write();
                        *c = previous_selection(*c);
                    } else {
                        let mut c = cursor.write();
                        *c = previous_selection(*c);
                        switch_active_alias(*c);   // 选中即写 active_alias 并持久化
                    }
                }
                KeyCode::Down => {
                    if *right_focus.read() {
                        let mut c = right_cursor.write();
                        *c = next_selection(*c, FIELD_COUNT);
                    } else {
                        let mut c = cursor.write();
                        *c = next_selection(*c, PROFILE_KEYS.len());
                        switch_active_alias(*c);
                    }
                }
                KeyCode::Right => {
                    if *right_focus.read() {
                        edit_field(active_alias.clone(), *right_cursor.read(), true);
                        *render_version.write() += 1;
                    } else {
                        *right_focus.write() = true;
                    }
                }
                KeyCode::Left => {
                    if *right_focus.read() {
                        edit_field(active_alias.clone(), *right_cursor.read(), false);
                        *render_version.write() += 1;
                    } else {
                        *right_focus.write() = false;
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });
    // ... 渲染见 Step 3
}
```

- [ ] **Step 3: 渲染（左右两栏，用行前缀区分）**

左侧渲染 4 个 Profile 卡片；右侧渲染 5 个 K/V 行（值右对齐）。两栏均通过单列 `Line` 列表构造，用空格宽度分隔（面板宽度固定，左栏 34 列、右栏剩余）。核心片段：

```rust
// 左侧：profile 卡片
let active_idx = PROFILE_KEYS.iter().position(|k| *k == active_alias).unwrap_or(0);
for (i, key) in PROFILE_KEYS.iter().enumerate() {
    let cfg = PERI_CONFIG_HANDLE.get().map(|h| h.read().clone());
    let (provider_label, model_label, effort_label, window_label) = if let Some(cfg) = &cfg {
        let profile = cfg.config.profiles.get(key).unwrap();
        let prov = if profile.provider.is_empty() {
            cfg.config.providers.first()
        } else {
            cfg.config.providers.iter().find(|p| p.id == profile.provider)
        };
        let model = profile.model.clone().filter(|m| !m.is_empty()).or_else(|| {
            prov.and_then(|p| p.models.get_model(key)).map(str::to_string).filter(|m| !m.is_empty())
        }).unwrap_or_else(|| key.to_string());
        let window = if profile.context_1m { "1m" } else { "200k" };
        (prov.map(|p| p.display_name().to_string()).unwrap_or_else(|| profile.provider.clone()), model, profile.effort.clone(), window.to_string())
    } else { (String::new(), key.to_string(), "xhigh".into(), "200k".into()) };
    let is_active = i == active_idx;
    let mark = if is_active { "●" } else { "○" };
    lines.push(Line::from(vec![
        Span::styled(format!(" {} {} · {}", mark, key, provider_label), if is_active {
            Style::new().fg(theme_def.read().semantic.status.success).bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.primary)
        }),
    ]));
    // 模型名行：含 effort 后缀的 high 用 model accent 色
    let model_spans = styled_model_name(&model_label, &theme_def);
    lines.push(Line::from(Span::raw("    ").pad_to_width(4)).with_spans(model_spans));
    // 摘要行：effort 用 effort 色，window 用 token 色
    lines.push(Line::from(vec![
        Span::styled("    ", Style::new()),
        Span::styled(effort_label, Style::new().fg(theme_def.read().semantic.effort).bold()),
        Span::styled(" · ", Style::new().fg(theme_def.read().semantic.text.muted)),
        Span::styled(window_label, Style::new().fg(theme_def.read().semantic.token_context)),
    ]));
}
```

右侧渲染（K/V 单行，值右对齐，无 `[]`）：

```rust
let right_pad = 24usize;
let rows: Vec<(&str, String)> = vec![
    ("Provider", provider_label),
    ("Model", model_label),
    ("Effort", current_effort),
    ("Max tokens", current_max_tokens.to_string()),
    ("1m enable", if current_ctx { "on" } else { "off" }.to_string()),
];
for (fi, (k, v)) in rows.iter().enumerate() {
    let is_focus = *right_focus.read() && fi == *right_cursor.read();
    let mark = if is_focus { "❯" } else { " " };
    let value_len = unicode_width::UnicodeWidthStr::width(v.as_str());
    let pad = right_pad.saturating_sub(value_len);
    lines.push(Line::from(vec![
        Span::styled(format!(" {} {} ", mark, k), if is_focus {
            Style::new().fg(theme_def.read().component.panel.title).bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.muted)
        }),
        Span::styled(format!("{}{}", " ".repeat(pad), v), Style::new().fg(theme_def.read().semantic.text.primary)),
    ]));
}
```

**注意 TUI-HOOK-001**：所有 `hooks.use_*` 必须在条件分支前按稳定顺序调用；`use_state`/`use_atom` 调用不允许出现在 `match`/`if` 内。

- [ ] **Step 4: 编辑函数（切换即写入并持久化）**

```rust
fn edit_field(alias: String, field: usize, forward: bool) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else { return };
    let mut cfg = handle.write();
    let next_provider_opt;
    {
        let alias_str = alias.as_str();
        let Some(profile) = cfg.config.profiles.get_mut(alias_str) else { return };
        match field {
            FIELD_PROVIDER => {
                let ids: Vec<String> = cfg.config.providers.iter().map(|p| p.id.clone()).collect();
                if ids.is_empty() { return; }
                let idx = ids.iter().position(|i| *i == profile.provider).unwrap_or(0);
                let next = ids[(idx + if forward { 1 } else { ids.len() - 1 }) % ids.len()].clone();
                profile.provider = next.clone();
                next_provider_opt = Some(next);
            }
            FIELD_MODEL => {
                // 联动：仅在切换 provider 时重置 model；此处手动循环 provider 下模型名
                let provider = cfg.config.providers.iter().find(|p| p.id == profile.provider);
                let Some(provider) = provider else { return };
                let mut models: Vec<String> = provider.models.get_model(alias_str)
                    .map(|m| m.to_string())
                    .filter(|m| !m.is_empty())
                    .map(|m| vec![m])
                    .unwrap_or_default();
                let current = profile.model.clone().unwrap_or_default();
                if !models.contains(&current) && !current.is_empty() {
                    models.insert(0, current.clone());
                }
                if models.is_empty() { return; }
                let idx = models.iter().position(|m| *m == current).unwrap_or(0);
                let next = models[(idx + if forward { 1 } else { models.len() - 1 }) % models.len()].clone();
                profile.model = Some(next);
            }
            FIELD_EFFORT => {
                let cur = EFFORT_LEVELS.iter().position(|e| *e == profile.effort).unwrap_or(0);
                profile.effort = EFFORT_LEVELS[(cur + if forward { 1 } else { EFFORT_LEVELS.len() - 1 }) % EFFORT_LEVELS.len()].to_string();
            }
            FIELD_MAX_TOKENS => {
                let cur = MAX_TOKEN_PRESETS.iter().position(|v| *v == profile.max_tokens).unwrap_or(0);
                profile.max_tokens = MAX_TOKEN_PRESETS[(cur + if forward { 1 } else { MAX_TOKEN_PRESETS.len() - 1 }) % MAX_TOKEN_PRESETS.len()];
            }
            FIELD_CONTEXT_1M => { profile.context_1m = !profile.context_1m; }
            _ => return,
        }
    }
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
    // 推送 ACP 服务端 + 刷新 SERVICE_SNAPSHOT（模型名可能变化）
    let resolved = resolve_model_name_for_alias(&snap.config, &alias);
    let s_handle = SERVICE_SNAPSHOT.state();
    let mut svc = s_handle.read().clone();
    if alias == snap.config.active_alias {
        svc.model_name = resolved;
        let provider_type = snap.config.providers.iter().find(|p| p.id == snap.config.profiles.get(&alias).map(|pf| pf.provider.as_str()).unwrap_or("")).map(|p| p.provider_type.clone()).unwrap_or_default();
        if !provider_type.is_empty() { svc.provider_name = provider_type; }
    }
    *s_handle.write() = svc;
    tokio::spawn(async move {
        if let Some(client) = ACP_CLIENT_HANDLE.get()
            && let Err(e) = client.update_config(&snap).await
        { tracing::warn!(error = %e, "ModelPanel: update_config push failed"); }
    });
}
```

**注意**：`FIELD_PROVIDER` 切换时按同档位联动 Model——在写 `profile.provider = next` 后，用目标 provider 的 `models.get_model(alias_str)` 覆盖 `profile.model`（同档位映射为空时保持 None 触发回退）。具体实现：在 `FIELD_PROVIDER` 分支内，设置 `next_provider_opt` 后追加：

```rust
if let Some(next_p) = cfg.config.providers.iter().find(|p| p.id == next) {
    let mapped = next_p.models.get_model(alias_str).map(str::to_string).filter(|m| !m.is_empty());
    // 同档位存在 → 覆盖 profile.model；否则保留手动 model（无档位时用 provider 默认）
    profile.model = mapped;
}
```

- [ ] **Step 5: 迁移 `switch_alias`**

`switch_active_alias(idx)`：写 `active_alias`、保存、刷新 SERVICE_SNAPSHOT 与 ACP 推送（复用原 `switch_alias` 逻辑，仅把 `MODEL_ALIASES` 索引换成 `PROFILE_KEYS`）。

- [ ] **Step 6: 颜色辅助**

```rust
/// 模型名内嵌 effort 后缀（如 "gpt-5.6-luna high"）用 model accent 色
fn styled_model_name(model: &str, theme: &ThemeDefinition) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let lower = model.to_lowercase();
    for level in [" low", " medium", " high", " xhigh", " max"] {
        if let Some(pos) = lower.rfind(level) {
            let (head, tail) = model.split_at(pos + 1);
            spans.push(Span::styled(head.to_string(), Style::new().fg(theme.semantic.text.primary)));
            spans.push(Span::styled(tail.to_string(), Style::new().fg(theme.semantic.model_accent).bold()));
            return spans;
        }
    }
    spans.push(Span::styled(model.to_string(), Style::new().fg(theme.semantic.text.primary)));
    spans
}
```

- [ ] **Step 7: 编译 + 运行测试**

```bash
cargo build -p peri-tui
cargo test -p peri-tui --lib
```
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add peri-tui/src/kit/panels/model.rs
git commit -m "feat: Model Panel 重构为左右分栏 Profile 编辑器"
```

### Task 14: 颜色语义（theme）

**Files:**
- Modify: `peri-theme/src/semantic.rs`、`peri-theme/src/loader.rs`、`peri-theme/src/bridge.rs`、`peri-theme/src/builtin/`（dark/light）
- Test: `peri-theme/tests/test_builtin.rs`

- [ ] **Step 1: 新增语义 token**

`SemanticTokens` 增加 `model_accent: Color`、`effort: Color`、`token_context: Color`：
- `model_accent`：模型名内嵌 effort 后缀色（如紫 `#A2A9E4`，独立于模型名主色 `model_info`）；
- `effort`：摘要/面板中 effort 值色（如橙 `#E5A46B`）；
- `token_context`：`200k`/`1m` 标识色（如青 `#7FB5D9`）。

`loader.rs` 从 `semantic.model_accent` / `semantic.effort` / `semantic.token_context` 读取；`bridge.rs` 透传；dark/light 主题各补色值。

- [ ] **Step 2: 测试**

`peri-theme/tests/test_builtin.rs` 增加断言三个新 token 非默认色。

- [ ] **Step 3: Commit**

```bash
git add peri-theme
git commit -m "feat: theme 新增 model_accent/effort/token_context 语义色"
```

---

## 阶段 6：E2E 与文档

### Task 15: E2E model-switch 测试更新

**Files:**
- Modify: `e2e/tests/panels/model-switch.test.ts`

- [ ] **Step 1: 更新交互脚本**

原测试：Enter 切换 alias。新测试：`↑/↓` 切换 profile（断言左侧第一行 `fable · <provider>`），`→` 进入右侧，`→` 切换 Effort 值（断言右侧 Effort 值变化），`Esc` 关闭。模型面板打开命令 `/model` 不变。

- [ ] **Step 2: 运行 E2E（参照 `e2e/CLAUDE.md` 命令）**

- [ ] **Step 3: Commit**

```bash
git add e2e/tests/panels/model-switch.test.ts
git commit -m "test: 更新 model panel e2e 为 profile 左右分栏交互"
```

### Task 16: 文档同步

**Files:**
- Modify: `TUI-PAGE.md`（已含初始设计，复核一致性）
- Modify: `TUI-STYLE.md:309-337`（`/model` 面板样式：新增左栏 profile 卡片、右侧 K/V、model_accent/effort/token_context 色表）
- Modify: `spec/global/domains/tui/tui-index.md:12,34`（面板列表 `ModelPanel` 描述）
- Modify: `peri-tui/CLAUDE.md`（如提到 `active_provider_id`/`thinking` 需更新）

- [ ] **Step 1: 更新 `TUI-STYLE.md`**

替换 `/model` 面板样式章节为左右分栏描述，新增色表：

| 元素 | 颜色 |
|---|---|
| 左侧 active profile（`●` + 标题行） | SAGE + BOLD |
| 左侧普通 profile（`○`） | TEXT |
| 模型名主色 | MODEL_INFO |
| 模型名内嵌 effort 后缀 | model_accent |
| 摘要 effort 值 | effort |
| `200k`/`1m` 标识 | token_context |
| 右侧 K/V key（焦点行） | THINKING + BOLD |
| 右侧 K/V value | TEXT |

- [ ] **Step 2: 更新 `tui-index.md` / `peri-tui/CLAUDE.md`**

- [ ] **Step 3: Commit**

```bash
git add TUI-PAGE.md TUI-STYLE.md spec/global/domains/tui/tui-index.md peri-tui/CLAUDE.md
git commit -m "docs: 同步 Model Profile 面板设计与样式文档"
```

---

## 自检（Self-Review）

**Spec 覆盖检查：**
- [ ] Profile 独立五字段 + 四档固定：Task 2/3
- [ ] `ProviderModels.fable` 空回退 opus：Task 1
- [ ] 废弃 thinking/active_provider_id/context_1m：Task 3/7/12
- [ ] 项目级 Profile 整体替换：Task 3
- [ ] `budget_tokens = max_tokens`：Task 4
- [ ] Profile 是请求唯一事实源：Task 5/6/7
- [ ] Model Panel 左右分栏 + 立即持久化 + Provider 联动 + 任意 Model：Task 13
- [ ] 显示名 `gpt-5.6-luna high` + 两种 high 不同色：Task 13/14
- [ ] Login 激活写 active profile：Task 11
- [ ] 旧配置迁移（extra 吸收 + 初始值）：Task 3（serde 兼容测试）
- [ ] E2E + 文档：Task 15/16

**类型一致性：**
- `Profiles::ALL`（Task 2）在 Task 3/6/9/12 引用；`ProfileConfig`（Task 2）在 Task 3/5/13 引用。
- `LlmProvider::effort_key()`（Task 4）替代 `thinking_key`；`LlmProvider::context_1m()`（Task 4）在 Task 7/12 引用。
- `apply_profile_effort`（Task 6）替代 `apply_thinking_effort`。
- `service_snapshot::derive_provider_and_model`（Task 8）与 `model.rs::resolve_model_name_for_alias`（Task 13）语义一致（Profile.model > models 映射 > alias）。
