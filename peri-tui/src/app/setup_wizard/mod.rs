use crate::app::FieldTextarea;

/// 向导步骤
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetupStep {
    /// 选择来源
    Choose,
    /// 选择语言
    Language,
    /// 合并表单：多 Provider + API Key + Model Aliases
    Form,
    /// 确认完成
    Done,
}

/// 配置来源选择
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetupSource {
    /// 手动输入 Custom API
    CustomApi,
    /// 从 Claude Code 迁移
    MigrateClaudeCode,
}

impl SetupSource {
    pub const ALL: [Self; 2] = [Self::CustomApi, Self::MigrateClaudeCode];

    pub fn label(&self, lc: &crate::i18n::LcRegistry) -> String {
        match self {
            Self::CustomApi => lc.tr("setup-source-custom-api"),
            Self::MigrateClaudeCode => lc.tr("setup-source-migrate"),
        }
    }

    pub fn description(&self, lc: &crate::i18n::LcRegistry) -> String {
        match self {
            Self::CustomApi => lc.tr("setup-source-custom-desc"),
            Self::MigrateClaudeCode => lc.tr("setup-source-migrate-desc"),
        }
    }
}

/// 支持的语言选项：(code, display_name)
pub const LANGUAGE_OPTIONS: [(&str, &str); 2] = [("en", "English"), ("zh-CN", "中文")];

/// Provider 类型选择
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderType {
    Anthropic,
    OpenAiCompatible,
}

impl ProviderType {
    pub fn label(&self, lc: &crate::i18n::LcRegistry) -> String {
        match self {
            Self::Anthropic => lc.tr("setup-provider-anthropic"),
            Self::OpenAiCompatible => lc.tr("setup-provider-openai"),
        }
    }

    pub fn type_str(&self) -> &str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai",
        }
    }

    pub fn cycle(&mut self) {
        *self = match self {
            Self::Anthropic => Self::OpenAiCompatible,
            Self::OpenAiCompatible => Self::Anthropic,
        };
    }

    pub fn default_provider_id(&self) -> &str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai",
        }
    }

    pub fn default_base_url(&self) -> &str {
        match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::OpenAiCompatible => "https://api.openai.com/v1",
        }
    }

    pub fn default_model_ids(&self) -> [&str; 3] {
        match self {
            Self::Anthropic => [
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-haiku-4-5-20251001",
            ],
            Self::OpenAiCompatible => ["gpt-5.5", "gpt-4o", "gpt-4o-mini"],
        }
    }
}

/// 单个别名的配置
#[derive(Debug, Clone)]
pub struct AliasConfig {
    pub field_model_id: FieldTextarea,
}

/// 单个 Provider 的完整表单数据
#[derive(Debug, Clone)]
pub struct MigratedProvider {
    pub provider_type: ProviderType,
    pub field_provider_id: FieldTextarea,
    pub field_base_url: FieldTextarea,
    pub field_api_key: FieldTextarea,
    pub aliases: [AliasConfig; 3],
    /// 勾选框状态：是否包含在最终保存中
    pub selected: bool,
}

impl MigratedProvider {
    /// 创建指定类型的默认 provider
    pub fn new(pt: ProviderType) -> Self {
        let mut field_provider_id = FieldTextarea::single_line();
        field_provider_id.set_value(pt.default_provider_id());
        let mut field_base_url = FieldTextarea::single_line();
        field_base_url.set_value(pt.default_base_url());
        Self {
            provider_type: pt,
            field_provider_id,
            field_base_url,
            field_api_key: FieldTextarea::single_line(),
            aliases: pt.default_model_ids().map(|s| {
                let mut f = FieldTextarea::single_line();
                f.set_value(s);
                AliasConfig { field_model_id: f }
            }),
            selected: true,
        }
    }

    /// 切换 Provider 类型后刷新默认值（保留 api_key）
    pub fn refresh_provider_defaults(&mut self) {
        self.field_provider_id
            .set_value(self.provider_type.default_provider_id());
        self.field_base_url
            .set_value(self.provider_type.default_base_url());
        self.aliases = self.provider_type.default_model_ids().map(|s| {
            let mut f = FieldTextarea::single_line();
            f.set_value(s);
            AliasConfig { field_model_id: f }
        });
    }

    /// 字段是否完整（provider_id 和 api_key 非空）
    pub fn is_complete(&self) -> bool {
        !self.field_provider_id.value().trim().is_empty()
            && !self.field_api_key.value().trim().is_empty()
            && self
                .aliases
                .iter()
                .all(|a| !a.field_model_id.value().trim().is_empty())
    }
}

/// Form 步骤的模式：浏览列表 vs 编辑详情
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormMode {
    /// 浏览列表：只读摘要，Space 勾选，Enter 进入编辑
    Browse,
    /// 编辑详情：可编辑字段，最后一个 Confirm 返回列表
    Edit,
}

/// 编辑模式下的可聚焦字段
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormField {
    ProviderType,
    ProviderId,
    BaseUrl,
    /// 测试 Base URL 连通性（Enter 触发 TCP 连接测试，结果单行显示）
    TestConnectivity,
    ApiKey,
    OpusModel,
    SonnetModel,
    HaikuModel,
    Confirm,
}

impl FormField {
    pub fn next(&self) -> Self {
        match self {
            Self::ProviderType => Self::ProviderId,
            Self::ProviderId => Self::BaseUrl,
            Self::BaseUrl => Self::TestConnectivity,
            Self::TestConnectivity => Self::ApiKey,
            Self::ApiKey => Self::OpusModel,
            Self::OpusModel => Self::SonnetModel,
            Self::SonnetModel => Self::HaikuModel,
            Self::HaikuModel => Self::Confirm,
            Self::Confirm => Self::ProviderType,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::ProviderType => Self::Confirm,
            Self::ProviderId => Self::ProviderType,
            Self::BaseUrl => Self::ProviderId,
            Self::TestConnectivity => Self::BaseUrl,
            Self::ApiKey => Self::TestConnectivity,
            Self::OpusModel => Self::ApiKey,
            Self::SonnetModel => Self::OpusModel,
            Self::HaikuModel => Self::SonnetModel,
            Self::Confirm => Self::HaikuModel,
        }
    }

    /// 是否为文本输入字段（可编辑）
    pub fn is_text_input(&self) -> bool {
        matches!(
            self,
            Self::ProviderId
                | Self::BaseUrl
                | Self::ApiKey
                | Self::OpusModel
                | Self::SonnetModel
                | Self::HaikuModel
        )
    }
}

/// Setup Wizard 全屏面板状态
pub struct SetupWizardPanel {
    pub step: SetupStep,
    /// Step 1: 来源选择
    pub source: SetupSource,
    pub choose_cursor: usize,
    /// Step 2: 语言选择
    pub language: String,
    pub language_cursor: usize,
    /// Step 3: 多 provider 列表
    pub providers: Vec<MigratedProvider>,
    /// 当前聚焦的 provider 索引（Edit 模式下使用）
    pub active_provider: usize,
    /// Form 步骤模式
    pub form_mode: FormMode,
    /// Browse 模式下的光标（0..providers.len()=providers, providers.len()=Submit）
    pub browse_cursor: usize,
    /// Edit 模式下的聚焦字段
    pub form_focus: FormField,
    /// 是否由 /setup 命令打开（false = 启动时无 Provider 自动触发）
    pub from_command: bool,
    /// Browse Submit 失败时的提示消息（下次操作自动清除）
    pub submit_error: Option<String>,
    /// 连通性测试结果（bool=成功, String=描述信息）
    pub connectivity_result: Option<(bool, String)>,
    /// 测试用 home_dir 覆盖（None 时走 `dirs_next::home_dir()` 全局查找）。
    ///
    /// Constructor Injection：避免测试依赖全局 `~/.claude/settings.json`，
    /// 单元测试用 TempDir 隔离即可覆盖迁移路径。
    pub home_dir_override: Option<std::path::PathBuf>,
}

impl Default for SetupWizardPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupWizardPanel {
    pub fn new() -> Self {
        Self {
            step: SetupStep::Language,
            source: SetupSource::CustomApi,
            choose_cursor: 0,
            language: "en".to_string(),
            language_cursor: 0,
            providers: vec![MigratedProvider::new(ProviderType::Anthropic)],
            active_provider: 0,
            form_mode: FormMode::Browse,
            browse_cursor: 0,
            form_focus: FormField::ProviderType,
            from_command: false,
            submit_error: None,
            connectivity_result: None,
            home_dir_override: None,
        }
    }

    /// 由 /setup 命令打开的 wizard（Esc 仅关闭向导，不退出应用）
    pub fn new_from_command() -> Self {
        Self {
            from_command: true,
            ..Self::new()
        }
    }

    /// 粘贴文本到当前聚焦的字段（仅保留第一行）
    pub fn paste_text(&mut self, text: &str) {
        if self.step != SetupStep::Form || self.form_mode != FormMode::Edit {
            return;
        }
        let mp = match self.providers.get_mut(self.active_provider) {
            Some(p) => p,
            None => return,
        };
        let text = text.lines().next().unwrap_or("");
        if self.form_focus.is_text_input() {
            if let Some(field) = ops::provider_field_buf(mp, self.form_focus) {
                field.insert_text(text);
            }
        }
    }

    /// 从 Claude Code 配置迁移，生成多 provider 列表
    ///
    /// 读取 `~/.claude/settings.json` 的 `env` 字段，按前缀检测凭据：
    /// - `ANTHROPIC_` → Anthropic provider
    /// - `OPENAI_` / `CODEX_` → OpenAI Compatible provider
    ///
    /// 同步字段：API_KEY、BASE_URL、DEFAULT_OPUS/SONNET/HAIKU_MODEL
    ///
    /// CODEX 前缀使用与 OPENAI 相同的默认 provider_id（"openai"）和 key 名检测逻辑。
    pub fn migrate_from_claude_code(&mut self) -> bool {
        let home = self
            .home_dir_override
            .clone()
            .or_else(dirs_next::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let claude_dir = home.join(".claude");
        let settings_path = claude_dir.join("settings.json");
        if !settings_path.exists() {
            return false;
        }
        let content = match std::fs::read_to_string(&settings_path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let val: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let env = match val.get("env").and_then(|e| e.as_object()) {
            Some(e) => e,
            None => return false,
        };

        let mut detected: Vec<MigratedProvider> = Vec::new();

        // 定义要检测的前缀及其对应的 provider 类型和默认 provider id
        let prefixes: &[(&str, ProviderType, &str, &[&str])] = &[
            (
                "ANTHROPIC",
                ProviderType::Anthropic,
                "anthropic",
                &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
            ),
            (
                "OPENAI",
                ProviderType::OpenAiCompatible,
                "openai",
                &["OPENAI_API_KEY"],
            ),
            (
                "CODEX",
                ProviderType::OpenAiCompatible,
                "openai",
                &["CODEX_API_KEY"],
            ),
        ];

        for &(prefix, pt, default_id, key_names) in prefixes {
            // 按优先级尝试多个 key 名
            let api_key = key_names
                .iter()
                .map(|k| env_get(env, k))
                .find(|v| !v.is_empty())
                .unwrap_or_default();
            let base_url = env_get(env, &format!("{}_BASE_URL", prefix));
            let opus = env_get(env, &format!("{}_DEFAULT_OPUS_MODEL", prefix));
            let sonnet = env_get(env, &format!("{}_DEFAULT_SONNET_MODEL", prefix));
            let haiku = env_get(env, &format!("{}_DEFAULT_HAIKU_MODEL", prefix));

            // 至少有 API key 或 base_url 才生成条目
            if api_key.is_empty() && base_url.is_empty() {
                continue;
            }

            let mut mp = MigratedProvider::new(pt);
            mp.field_provider_id.set_value(default_id);

            if !api_key.is_empty() {
                mp.field_api_key.set_value(&api_key);
            } else {
                // 无 API key → 默认不选中
                mp.selected = false;
            }

            if !base_url.is_empty() {
                mp.field_base_url.set_value(&base_url);
            }

            if !opus.is_empty() {
                mp.aliases[0].field_model_id.set_value(&opus);
            }
            if !sonnet.is_empty() {
                mp.aliases[1].field_model_id.set_value(&sonnet);
            }
            if !haiku.is_empty() {
                mp.aliases[2].field_model_id.set_value(&haiku);
            }

            detected.push(mp);
        }

        if detected.is_empty() {
            return false;
        }

        self.providers = detected;
        self.active_provider = 0;
        self.form_mode = FormMode::Browse;
        self.browse_cursor = 0;
        self.form_focus = FormField::ProviderType;
        true
    }
}

/// 从 env JSON 对象中读取字符串值，不存在或非字符串返回空串并告警
fn env_get(env: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    match env.get(key) {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) => {
            tracing::warn!(
                "setup wizard: env key '{}' has non-string value (type {:?}), skipping",
                key,
                v
            );
            String::new()
        }
        None => String::new(),
    }
}

/// 向 Provider 端点发送最小 HTTP GET 请求测试联通性（5 秒超时）
///
/// 向 base_url 发送 HTTP/1.0 GET 请求，检查服务器是否有任何响应。
/// 返回 `(成功标志, 结果描述)`。
pub(crate) fn test_connectivity(base_url: &str) -> (bool, String) {
    use std::io::{Read, Write};

    if base_url.trim().is_empty() {
        return (false, "Base URL is empty".to_string());
    }

    let (host, port, path) = match parse_url_parts(base_url) {
        Some(p) => p,
        None => return (false, format!("Invalid URL: {}", base_url)),
    };

    let addr_str = format!("{}:{}", host, port);
    use std::net::ToSocketAddrs;
    let addr = match addr_str.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => return (false, format!("DNS resolution failed for {}", host)),
    };

    let timeout = std::time::Duration::from_secs(5);
    let mut stream = match std::net::TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => return (false, format!("{} unreachable: {}", host, e)),
    };
    let _ = stream.set_read_timeout(Some(timeout));

    // 发送最小 HTTP/1.0 GET 请求
    let req = format!("GET {} HTTP/1.0\r\nHost: {}\r\n\r\n", path, host);
    if stream.write_all(req.as_bytes()).is_err() {
        return (false, format!("{} connected but send failed", host));
    }

    // 读取至少 1 字节即视为响应成功
    let mut buf = [0u8; 1];
    match stream.read_exact(&mut buf) {
        Ok(()) => (true, format!("{} reachable", base_url)),
        Err(e) => (false, format!("{} no response: {}", host, e)),
    }
}

/// 解析 URL 部件：`(host, port, path)`，默认端口 https→443, http→80
fn parse_url_parts(url: &str) -> Option<(&str, u16, &str)> {
    let s = url.trim();
    let (scheme, rest) = if let Some(idx) = s.find("://") {
        (&s[..idx], &s[idx + 3..])
    } else {
        ("https", s)
    };
    let default_port: u16 = if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        443
    };
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port_str) = match host_port.rfind(':') {
        Some(idx) if host_port[idx + 1..].chars().all(|c| c.is_ascii_digit()) => {
            (&host_port[..idx], &host_port[idx + 1..])
        }
        _ => (host_port, ""),
    };
    if host.is_empty() {
        return None;
    }
    let port: u16 = if port_str.is_empty() {
        default_port
    } else {
        port_str.parse().ok()?
    };
    Some((host, port, path))
}

pub use ops::{
    build_wizard_config, handle_setup_wizard_key, needs_setup, save_setup, save_setup_to,
    SetupWizardAction,
};

mod ops;

#[cfg(test)]
#[path = "setup_wizard_test.rs"]
mod tests;
