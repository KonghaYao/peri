# 状态栏模型名称字段修复实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复状态栏 provider/model 字段：将短别名替换为完整模型名，并将 provider/model 渲染为统一样式的整体文本。

**Architecture:** 在 `derive_provider_and_model()` 中新增 model_name 派生（从 `AppConfig.providers[].models` 查询），通过 `ServiceSnapshot.model_name` 字段传递到 StatusBar 组件，StatusBar 以单一样式渲染 `provider_name/model_name`。现有的 `model_alias` 字段保持不变（Model/Agent/Status Panel 等组件需要短别名作标签）。SetupWizard 同步更新为使用 model_name。

**Tech Stack:** Rust, ratatui-kit

---

### Task 1: 更新 `derive_provider_and_model` 返回实际 model_name

**Files:**
- Modify: `peri-tui/src/kit/service_snapshot.rs:397-411`
- Modify: `peri-tui/src/kit/service_snapshot.rs:635-667`（测试）

- [ ] **Step 1: 修改函数签名和实现**

将 `derive_provider_and_model` 从返回 `(provider_type, active_alias)` 改为返回 `(provider_type, active_alias, model_name)`。

`peri-tui/src/kit/service_snapshot.rs:397-411` 当前代码：

```rust
fn derive_provider_and_model(peri_config: &SharedPeriConfig) -> (String, String) {
    let cfg = peri_config.read();
    let active_id = cfg.config.active_provider_id.clone();
    let active_alias = cfg.config.active_alias.clone();

    let provider_type = cfg
        .config
        .providers
        .iter()
        .find(|p| p.id == active_id)
        .map(|p| p.provider_type.clone())
        .unwrap_or(active_id);

    (provider_type, active_alias)
}
```

替换为：

```rust
/// 从 PeriConfig 派生 (provider_type, active_alias, model_name)。
/// model_name 优先从 provider.models.get_model(alias) 查询；
/// 若 provider 未配置 models 或 alias 非标准名，回退到 active_alias。
fn derive_provider_and_model(peri_config: &SharedPeriConfig) -> (String, String, String) {
    let cfg = peri_config.read();
    let active_id = cfg.config.active_provider_id.clone();
    let active_alias = cfg.config.active_alias.clone();

    let provider = cfg.config.providers.iter().find(|p| p.id == active_id);

    let provider_type = provider
        .map(|p| p.provider_type.clone())
        .unwrap_or(active_id.clone());

    let model_name = provider
        .and_then(|p| p.models.get_model(&active_alias))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| active_alias.clone());

    (provider_type, active_alias, model_name)
}
```

- [ ] **Step 2: 更新调用点 `tick_once`**

`peri-tui/src/kit/service_snapshot.rs:130` 当前代码：

```rust
let (provider_name, model_alias) = derive_provider_and_model(&src.peri_config);
```

替换为：

```rust
let (provider_name, model_alias, model_name) = derive_provider_and_model(&src.peri_config);
```

在 `snap` 构造中新增 `model_name` 字段（见 Task 2）。

- [ ] **Step 3: 更新测试**

`peri-tui/src/kit/service_snapshot.rs:635-667` 当前测试：

```rust
#[tokio::test]
async fn test_derive_provider_and_model_default() {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let (provider, model) = derive_provider_and_model(&peri_config);
    // 默认 AppConfig::default() 的 active_alias 和 active_provider_id 均为空
    assert!(provider.is_empty());
    assert!(model.is_empty());
}

#[tokio::test]
async fn test_derive_provider_and_model_set() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "sonnet".into(),
            active_provider_id: "p1".into(),
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, model) = derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(model, "sonnet");
}
```

替换为：

```rust
#[tokio::test]
async fn test_derive_provider_and_model_default() {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let (provider, alias, model_name) = derive_provider_and_model(&peri_config);
    // 默认 AppConfig::default() 的 active_alias 和 active_provider_id 均为空
    assert!(provider.is_empty());
    assert!(alias.is_empty());
    assert!(model_name.is_empty());
}

#[tokio::test]
async fn test_derive_provider_and_model_set() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig, ProviderModels};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "sonnet".into(),
            active_provider_id: "p1".into(),
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                models: ProviderModels {
                    opus: "claude-opus-4-20250514".into(),
                    sonnet: "claude-sonnet-4-20250514".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, alias, model_name) = derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(alias, "sonnet");
    assert_eq!(model_name, "claude-sonnet-4-20250514");
}

#[tokio::test]
async fn test_derive_provider_and_model_no_models_fallback() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "custom-alias".into(),
            active_provider_id: "p1".into(),
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, alias, model_name) = derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(alias, "custom-alias");
    // providers.models 为空，回退到 active_alias
    assert_eq!(model_name, "custom-alias");
}
```

- [ ] **Step 4: 编译验证**

```bash
cargo check -p peri-tui
```

预期：`derive_provider_and_model` 相关调用点有编译错误（因为返回值从 2 元组变 3 元组，还有 `snap.model_name` 尚未定义），这是预期的——后续任务会解决。

---

### Task 2: 添加 `model_name` 字段到 `ServiceSnapshot`

**Files:**
- Modify: `peri-tui/src/kit/atoms.rs:55-66`
- Modify: `peri-tui/src/kit/service_snapshot.rs:218-228`

- [ ] **Step 1: 在 `ServiceSnapshot` 中新增字段**

`peri-tui/src/kit/atoms.rs:55-66` 当前代码：

```rust
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceSnapshot {
    pub cwd: String,
    pub provider_name: String,
    pub model_alias: String,
    pub permission_mode: String,
    pub memory_mb: u64,
    pub cpu_percent: f32,
    pub mcp: McpStatusSnapshot,
    pub cron_total: usize,
    pub cron_enabled: usize,
}
```

在 `model_alias` 之后新增 `model_name` 字段：

```rust
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceSnapshot {
    pub cwd: String,
    pub provider_name: String,
    pub model_alias: String,
    pub model_name: String,
    pub permission_mode: String,
    pub memory_mb: u64,
    pub cpu_percent: f32,
    pub mcp: McpStatusSnapshot,
    pub cron_total: usize,
    pub cron_enabled: usize,
}
```

`Default` derive 会自动生成 `model_name: String::default()` → 空字符串。

- [ ] **Step 2: 在 `tick_once` 中填充 `model_name`**

`peri-tui/src/kit/service_snapshot.rs:218-228` 当前代码：

```rust
let snap = ServiceSnapshot {
    cwd: src.cwd.clone(),
    provider_name,
    model_alias,
    permission_mode: permission_mode.to_string(),
    memory_mb,
    cpu_percent: cpu_percent.round(),
    mcp,
    cron_total,
    cron_enabled,
};
```

替换为：

```rust
let snap = ServiceSnapshot {
    cwd: src.cwd.clone(),
    provider_name,
    model_alias,
    model_name,
    permission_mode: permission_mode.to_string(),
    memory_mb,
    cpu_percent: cpu_percent.round(),
    mcp,
    cron_total,
    cron_enabled,
};
```

- [ ] **Step 3: 编译验证**

```bash
cargo check -p peri-tui
```

---

### Task 3: 更新 StatusBar 渲染逻辑

**Files:**
- Modify: `peri-tui/src/kit/status_bar.rs:55-71`（渲染逻辑）
- Modify: `peri-tui/src/kit/status_bar.rs:279-322`（测试）

- [ ] **Step 1: 重写 provider/model 渲染**

`peri-tui/src/kit/status_bar.rs:55-71` 当前代码：

```rust
    // 3. provider/model
    spans.push(separator());
    if !snap.provider_name.is_empty() {
        let mut style = Style::default().fg(statusbar().muted);
        if provider_highlighted {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(format!(" {}", snap.provider_name), style));
        spans.push(Span::styled("/", Style::default().fg(statusbar().dim)));
    }
    if !snap.model_alias.is_empty() {
        let mut style = Style::default().fg(statusbar().text);
        if model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(snap.model_alias.clone(), style));
    }
```

替换为：

```rust
    // 3. provider/model —— 整体统一样式
    let model_display = if !snap.model_name.is_empty() {
        &snap.model_name
    } else if !snap.model_alias.is_empty() {
        &snap.model_alias
    } else {
        ""
    };

    if !snap.provider_name.is_empty() && !model_display.is_empty() {
        spans.push(separator());
        let mut style = Style::default().fg(statusbar().text);
        if provider_highlighted && model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        } else if provider_highlighted {
            style = style.add_modifier(Modifier::BOLD);
        } else if model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(
            format!(" {}/{}", snap.provider_name, model_display),
            style,
        ));
    }
```

- [ ] **Step 2: 更新 StatusBar 测试**

在两个测试用例中添加 `model_name` 字段：

`test_status_bar_row_renders_without_panic` (line 279):
```rust
*atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
    cwd: "/home/user/test-project".into(),
    provider_name: "anthropic".into(),
    model_alias: "sonnet".into(),
    model_name: "claude-sonnet-4-20250514".into(),
    permission_mode: "accept-edit".into(),
    memory_mb: 256,
    cpu_percent: 12.5,
    ..Default::default()
};
```

`test_status_bar_handles_empty_provider_model` (line 303):
```rust
*atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
    cwd: "/tmp".into(),
    provider_name: "".into(),
    model_alias: "".into(),
    model_name: "".into(),
    permission_mode: "default".into(),
    memory_mb: 0,
    cpu_percent: 0.0,
    ..Default::default()
};
// 并新增断言：
assert!(snap.model_name.is_empty());
```

- [ ] **Step 3: 编译与运行测试**

```bash
cargo check -p peri-tui && cargo test -p peri-tui --lib -- status_bar
```

---

### Task 4: 更新 SetupWizard 组件

**Files:**
- Modify: `peri-tui/src/kit/setup_wizard.rs`（:44 和 :73 附近）

- [ ] **Step 1: 使用 model_name 替代 model_alias 作展示**

读取 `peri-tui/src/kit/setup_wizard.rs` 中 `model_alias` 的使用位置，将其改为优先使用 `model_name`，fallback 到 `model_alias`。

```rust
let model_label = if !snapshot.model_name.is_empty() {
    &snapshot.model_name
} else {
    &snapshot.model_alias
};
```

并将 `format!("Provider: {} ({})", provider_name, model_alias)` 等引用替换为使用 `model_label`。

- [ ] **Step 2: 编译验证**

```bash
cargo check -p peri-tui
```

---

### Task 5: 运行完整测试套件

- [ ] **Step 1: 运行 peri-tui 全量测试**

```bash
cargo test -p peri-tui --lib
```

预期：所有测试 PASS。

- [ ] **Step 2: 运行完整编译检查**

```bash
cargo check -p peri-tui && cargo clippy -p peri-tui --lib
```

预期：无错误、无 clippy 警告。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/atoms.rs peri-tui/src/kit/service_snapshot.rs peri-tui/src/kit/status_bar.rs peri-tui/src/kit/setup_wizard.rs
git commit -m "fix(tui): status bar displays model_name instead of alias with unified style

- Add model_name field to ServiceSnapshot
- Update derive_provider_and_model to query ProviderModels.get_model()
- StatusBar renders provider/model_name as a single styled span
- SetupWizard uses model_name with model_alias fallback"
```
