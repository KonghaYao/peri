//! 认证、可信主体与连接实例绑定（WP-006 独占 scope）。
//!
//! 本模块是**唯一**允许产生 `PrincipalId` / `ClientInstanceId` 的地方（另一个来源是
//! WP-005 的 stdio 连接实例）。规则来自 accepted 设计：
//!
//! - `_meta.io.modelcontextprotocol/clientInfo` 与任何客户端自报字段（含 `instanceId`）
//!   都**不是**身份，只用于显示：它们可以被伪造，因此不参与授权判定。
//! - bearer 模式下主体只由「成功校验 token」这一事实产生；token 值本身绝不进入
//!   结构体字段、`Debug`、日志、错误文本或 wire。
//! - 连接实例是每次 TCP 连接生成的不可猜 opaque id；状态句柄绑定
//!   `(principal, client_instance)`，跨主体/跨实例一律拒绝。
//! - legacy 会话（`Mcp-Session-Id`）额外绑定创建它的主体与实例：会话 id 由 SDK 以
//!   128-bit 随机 UUID 生成，属于「会话持有者」凭证；跨主体使用同一会话 id 直接按
//!   「会话不存在」处理，不泄露存在性。
//!
//! 秘密处理：
//! - [`SecretToken`] 只在内存中保存字节，`Drop` 时清零，`Debug` 只打印 `<redacted>`，
//!   不实现 `Display` / `Serialize` / `PartialEq`（避免不经意比较或序列化）。
//! - 比较使用 `subtle` 的常量时间比较；长度不同会短路，泄露的仅是「长度不同」。
//! - 错误类型只携带来源（env 变量名 / 文件路径）与固定文案，不回显 caller 提供的值。
//!
//! 与 WP-005 的接口（冻结在 handoff 中）：HTTP 传输把 [`AuthenticatedPrincipal`] 放进
//! `http::Request` 的 extensions；SDK 会把它随 `http::request::Parts` 搬进 MCP 请求
//! 的 extensions，MCP core 用 [`principal_from_parts`]（或
//! [`principal_from_extensions`]）取出，绝不再自行构造 principal。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;

use http::{header, HeaderMap, StatusCode};
use parking_lot::RwLock;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::config::{AuthConfig, TokenSource};
use crate::wire::{ClientInstanceId, PrincipalId, RequestContext, RequestId};

/// 未配置 token（仅回环）时使用的固定主体。
///
/// 这不是「无身份」：它表示本机操作者。非回环绑定在配置校验阶段就要求 token 来源，
/// 因此这个主体永远不会服务非回环请求。
pub const LOOPBACK_PRINCIPAL: &str = "local-loopback";

/// bearer 主体的前缀（主体值由进程随机生成，与 token 值无派生关系）。
pub const BEARER_PRINCIPAL_PREFIX: &str = "bearer-";

/// 连接实例前缀，便于日志中一眼区分实例类型。
pub const CONNECTION_INSTANCE_PREFIX: &str = "http-conn-";

/// 单个进程最多记忆的 legacy 会话绑定数（防止无界增长）。
const MAX_BOUND_SESSIONS: usize = 1024;

/// 401 响应里 `WWW-Authenticate` 的值；不含任何主体或 token 信息。
///
/// realm 用产品名（`local-mcp-server`）：D-003 改名后它必须与
/// [`crate::mcp::server::SandboxServer::server_implementation`] 的 `serverInfo.name`
/// 一致，否则同一个进程会在两处暴露两个产品身份。
pub const WWW_AUTHENTICATE_VALUE: &str = "Bearer realm=\"local-mcp-server\"";

/// 认证方式（用于日志与测试断言，不进入 wire）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// 已通过 bearer token 校验。
    Bearer,
    /// 未配置 token 的回环模式。
    Loopback,
}

impl AuthMethod {
    /// 稳定标识（日志用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::Loopback => "loopback",
        }
    }
}

/// 认证层产出的可信主体；客户端无法构造（字段私有 + 只能由 [`Authenticator`] 生成）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    principal: PrincipalId,
    client_instance: ClientInstanceId,
    method: AuthMethod,
}

impl AuthenticatedPrincipal {
    /// 只允许认证层内部构造。
    fn new(
        principal: impl Into<PrincipalId>,
        client_instance: impl Into<ClientInstanceId>,
        method: AuthMethod,
    ) -> Self {
        Self {
            principal: principal.into(),
            client_instance: client_instance.into(),
            method,
        }
    }

    /// 主体 id。
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// 连接实例 id。
    pub fn client_instance(&self) -> &str {
        &self.client_instance
    }

    /// 认证方式。
    pub fn method(&self) -> AuthMethod {
        self.method
    }

    /// 换成另一连接实例（仅用于 legacy 会话绑定：实例由会话决定，主体不变）。
    pub fn with_client_instance(&self, instance: impl Into<ClientInstanceId>) -> Self {
        Self {
            principal: self.principal.clone(),
            client_instance: instance.into(),
            method: self.method,
        }
    }

    /// 构造 MCP 调用上下文（WP-005 的唯一入口；不再自行编造 principal）。
    pub fn to_request_context(&self, request_id: impl Into<RequestId>) -> RequestContext {
        RequestContext::new(
            request_id,
            self.principal.clone(),
            self.client_instance.clone(),
        )
    }
}

/// 把可信主体放进 HTTP 请求 extensions（HTTP 传输内部使用）。
pub fn install_principal(extensions: &mut http::Extensions, principal: AuthenticatedPrincipal) {
    extensions.insert(principal);
}

/// 从 extensions 取出可信主体。
pub fn principal_from_extensions(extensions: &http::Extensions) -> Option<&AuthenticatedPrincipal> {
    extensions.get::<AuthenticatedPrincipal>()
}

/// 从 MCP 请求携带的 `http::request::Parts` 取出可信主体。
///
/// SDK 的 Streamable HTTP 传输会把请求的 `Parts`（含本模块插入的
/// [`AuthenticatedPrincipal`]）放进 MCP 请求 extensions，因此 MCP core 可以直接取用。
pub fn principal_from_parts(parts: &http::request::Parts) -> Option<&AuthenticatedPrincipal> {
    principal_from_extensions(&parts.extensions)
}

/// 认证失败原因；对外只暴露一个状态码与固定文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRejection {
    /// 缺少 `Authorization` 头。
    MissingAuthorization,
    /// `Authorization` 头存在但不是合法的 `Bearer <token>` 形状。
    MalformedAuthorization,
    /// bearer token 与配置值不匹配。
    InvalidToken,
}

impl AuthRejection {
    /// 对应的 HTTP 状态码：统一 401，不区分具体原因（避免成为猜测 oracle）。
    pub fn status(&self) -> StatusCode {
        StatusCode::UNAUTHORIZED
    }

    /// `WWW-Authenticate` 头值。
    pub fn www_authenticate(&self) -> &'static str {
        WWW_AUTHENTICATE_VALUE
    }

    /// 可外发文案：三种原因共用一句，不回显任何提交内容。
    pub fn public_message(&self) -> &'static str {
        "Unauthorized: missing or invalid bearer token"
    }

    /// 日志用原因（同样不含秘密值）。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::MissingAuthorization => "missing authorization header",
            Self::MalformedAuthorization => "malformed authorization header",
            Self::InvalidToken => "bearer token mismatch",
        }
    }
}

/// token 来源加载失败（启动期，fail closed）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// 环境变量不存在或不是有效 UTF-8。
    #[error("token environment variable `{var}` is missing or not valid UTF-8")]
    EnvUnavailable {
        /// 环境变量名（不是值）。
        var: String,
    },
    /// 文件不可读。
    #[error("token file is unreadable: {reason}")]
    FileUnreadable {
        /// 不含 token 值的原因说明（只含 IO 错误类别）。
        reason: String,
    },
    /// 来源存在但内容为空（等于没有认证，必须拒绝）。
    #[error("token source `{origin}` resolved to an empty token")]
    EmptyToken {
        /// 来源描述（env 变量名或文件路径）。
        origin: String,
    },
}

/// 秘密 token 持有者：不实现 `Display`/`Serialize`，`Debug` 脱敏，`Drop` 清零。
pub struct SecretToken {
    bytes: Vec<u8>,
}

impl SecretToken {
    /// 从原始字节构造（空值被拒绝）。
    pub fn from_bytes(source: &str, bytes: Vec<u8>) -> Result<Self, AuthError> {
        if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Err(AuthError::EmptyToken {
                origin: source.to_string(),
            });
        }
        Ok(Self { bytes })
    }

    /// 从环境变量读取；只记录变量名，不记录值。
    pub fn from_env(var: &str) -> Result<Self, AuthError> {
        let raw = std::env::var(var).map_err(|_| AuthError::EnvUnavailable {
            var: var.to_string(),
        })?;
        Self::from_source_text(var, &raw)
    }

    /// 从文件读取；读取失败只报告 IO 类别，不回显文件内容。
    pub fn from_file(path: &Path, display: &str) -> Result<Self, AuthError> {
        warn_if_world_readable(path);
        let raw = std::fs::read_to_string(path).map_err(|err| AuthError::FileUnreadable {
            reason: err.kind().to_string(),
        })?;
        Self::from_source_text(display, &raw)
    }

    /// 统一处理来源文本：去掉首尾空白（含文件尾换行），然后校验非空。
    fn from_source_text(source: &str, raw: &str) -> Result<Self, AuthError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AuthError::EmptyToken {
                origin: source.to_string(),
            });
        }
        Ok(Self {
            bytes: trimmed.as_bytes().to_vec(),
        })
    }

    /// 常量时间比较；长度不同会短路（只泄露「长度不同」）。
    pub fn verify(&self, presented: &[u8]) -> bool {
        self.bytes.ct_eq(presented).into()
    }

    /// 长度（仅供测试与诊断；不泄露内容）。
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// 是否为空（构造时已拒绝空值，保留给 clippy 的 `len_without_is_empty`）。
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 只报告长度量级也不必要；固定文案，避免任何形式的推断。
        f.write_str("SecretToken(<redacted>)")
    }
}

impl Drop for SecretToken {
    fn drop(&mut self) {
        // 安全代码清零，不依赖 unsafe，也不引入 zeroize 依赖。
        self.bytes.fill(0);
        self.bytes.clear();
    }
}

/// 认证器：进程级唯一，启动时从配置的来源加载 token（失败即 fail closed）。
pub struct Authenticator {
    method: AuthMethod,
    principal: PrincipalId,
    token: Option<SecretToken>,
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 手工实现：token 只输出是否存在，绝不输出内容（SecretToken 自身已脱敏）。
        f.debug_struct("Authenticator")
            .field("method", &self.method)
            .field("principal", &self.principal)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Authenticator {
    /// 依据 [`AuthConfig`] 构造认证器。
    ///
    /// 未配置 token 来源表示「仅回环、无认证」；配置了来源但读不到（env 缺失、文件不可读、
    /// 内容为空）一律返回错误，由调用方 fail closed，绝不降级为无认证。
    pub fn from_config(config: &AuthConfig) -> Result<Self, AuthError> {
        match &config.token {
            None => Ok(Self {
                method: AuthMethod::Loopback,
                principal: LOOPBACK_PRINCIPAL.to_string(),
                token: None,
            }),
            Some(TokenSource::Env { var }) => Ok(Self {
                method: AuthMethod::Bearer,
                principal: mint_principal_id(),
                token: Some(SecretToken::from_env(var)?),
            }),
            Some(TokenSource::File { path }) => {
                let display = path.display().to_string();
                Ok(Self {
                    method: AuthMethod::Bearer,
                    principal: mint_principal_id(),
                    token: Some(SecretToken::from_file(path, &display)?),
                })
            }
        }
    }

    /// 认证方式。
    pub fn method(&self) -> AuthMethod {
        self.method
    }

    /// 本认证器产生的主体 id（同一进程内稳定）。
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// 该校验请求头并绑定连接实例。
    ///
    /// 返回的 [`AuthenticatedPrincipal`] 是唯一可信身份来源；客户端自报的任何
    /// `clientInfo` / `instanceId` 都不参与。
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        connection_instance: &ClientInstanceId,
    ) -> Result<AuthenticatedPrincipal, AuthRejection> {
        match (&self.method, &self.token) {
            (AuthMethod::Loopback, _) => {
                // 无认证模式：不解析 Authorization，也不因此获得任何额外权限。
                Ok(AuthenticatedPrincipal::new(
                    self.principal.clone(),
                    connection_instance.clone(),
                    AuthMethod::Loopback,
                ))
            }
            (AuthMethod::Bearer, Some(token)) => {
                let presented = bearer_credential(headers)?;
                if !token.verify(presented) {
                    return Err(AuthRejection::InvalidToken);
                }
                Ok(AuthenticatedPrincipal::new(
                    self.principal.clone(),
                    connection_instance.clone(),
                    AuthMethod::Bearer,
                ))
            }
            // 构造期保证 Bearer 必然带 token。
            (AuthMethod::Bearer, None) => Err(AuthRejection::InvalidToken),
        }
    }
}

/// 从 `Authorization` 头提取 bearer 凭证（只回传借用切片，不复制成新的缓冲区）。
fn bearer_credential(headers: &HeaderMap) -> Result<&[u8], AuthRejection> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(AuthRejection::MissingAuthorization)?;
    let text = value
        .to_str()
        .map_err(|_| AuthRejection::MalformedAuthorization)?;
    let mut parts = text.splitn(2, ' ');
    let scheme = parts.next().unwrap_or_default();
    let credential = parts
        .next()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .ok_or(AuthRejection::MalformedAuthorization)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthRejection::MalformedAuthorization);
    }
    Ok(credential.as_bytes())
}

/// 生成新的主体 id（进程随机，与 token 值无派生关系，因而不泄露任何 token 信息）。
fn mint_principal_id() -> PrincipalId {
    format!("{BEARER_PRINCIPAL_PREFIX}{}", Uuid::new_v4().simple())
}

/// 生成新的连接实例 id（不可猜，每条 TCP 连接一个）。
pub fn mint_client_instance() -> ClientInstanceId {
    format!("{CONNECTION_INSTANCE_PREFIX}{}", Uuid::new_v4().simple())
}

/// legacy 会话绑定表：会话 id → (主体, 连接实例)。
///
/// 会话 id 由 SDK 以 128-bit 随机 UUID 生成。绑定后：
/// - 同一主体（含同一主体的新连接）使用该会话时，实例固定为创建会话时的实例，
///   这就是「legacy session owner」语义；
/// - 不同主体使用同一会话 id 时调用方必须按「会话不存在」处理（不泄露存在性）。
#[derive(Debug, Default)]
pub struct SessionBindings {
    inner: RwLock<BindingTable>,
}

#[derive(Debug, Default)]
struct BindingTable {
    map: HashMap<String, SessionBinding>,
    order: VecDeque<String>,
}

/// 单个会话的绑定内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    /// 创建会话的主体。
    pub principal: PrincipalId,
    /// 创建会话的连接实例。
    pub client_instance: ClientInstanceId,
}

impl SessionBindings {
    /// 空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 绑定（重复绑定同一会话时覆盖，`initialize` 重放场景下保持幂等）。
    pub fn bind(&self, session_id: &str, principal: &str, client_instance: &str) {
        let mut table = self.inner.write();
        if table.map.contains_key(session_id) {
            table.order.retain(|candidate| candidate != session_id);
        }
        table.map.insert(
            session_id.to_string(),
            SessionBinding {
                principal: principal.to_string(),
                client_instance: client_instance.to_string(),
            },
        );
        table.order.push_back(session_id.to_string());
        while table.order.len() > MAX_BOUND_SESSIONS {
            if let Some(evicted) = table.order.pop_front() {
                table.map.remove(&evicted);
            }
        }
    }

    /// 查询绑定。
    pub fn resolve(&self, session_id: &str) -> Option<SessionBinding> {
        self.inner.read().map.get(session_id).cloned()
    }

    /// 解绑（客户端 DELETE 关闭会话）。
    pub fn unbind(&self, session_id: &str) {
        let mut table = self.inner.write();
        if table.map.remove(session_id).is_some() {
            table.order.retain(|candidate| candidate != session_id);
        }
    }

    /// 当前绑定数量。
    pub fn len(&self) -> usize {
        self.inner.read().map.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 在 unix 上对过于宽松的 token 文件权限给出**警告**（不阻断启动）。
#[cfg(unix)]
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", mode & 0o777),
            "token 文件权限过于宽松（group/other 可读），建议 chmod 600"
        );
    }
}

/// 非 unix 平台不做权限判断。
#[cfg(not(unix))]
fn warn_if_world_readable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("header value"),
        );
        headers
    }

    #[test]
    fn loopback_mode_ignores_authorization_header() {
        let authenticator =
            Authenticator::from_config(&AuthConfig::default()).expect("loopback authenticator");
        let instance = mint_client_instance();
        let principal = authenticator
            .authenticate(&bearer_headers("anything"), &instance)
            .expect("loopback 模式不校验 Authorization");
        assert_eq!(principal.principal(), LOOPBACK_PRINCIPAL);
        assert_eq!(principal.client_instance(), instance);
        assert_eq!(principal.method(), AuthMethod::Loopback);
    }

    #[test]
    fn bearer_mode_requires_exact_token_and_reports_fixed_reason() {
        let token = format!("tok-{}", Uuid::new_v4().simple());
        let secret = SecretToken::from_bytes("test", token.clone().into_bytes()).expect("secret");
        let authenticator = Authenticator {
            method: AuthMethod::Bearer,
            principal: mint_principal_id(),
            token: Some(secret),
        };
        let instance = mint_client_instance();

        let ok = authenticator
            .authenticate(&bearer_headers(&token), &instance)
            .expect("正确 token");
        assert_eq!(ok.method(), AuthMethod::Bearer);
        assert_eq!(ok.principal(), authenticator.principal());

        let missing = authenticator
            .authenticate(&HeaderMap::new(), &instance)
            .expect_err("缺少头必须拒绝");
        assert_eq!(missing, AuthRejection::MissingAuthorization);

        let malformed = authenticator
            .authenticate(&bearer_headers(""), &instance)
            .expect_err("空凭证必须拒绝");
        assert_eq!(malformed, AuthRejection::MalformedAuthorization);

        let wrong = authenticator
            .authenticate(&bearer_headers("not-the-token"), &instance)
            .expect_err("错误 token 必须拒绝");
        assert_eq!(wrong, AuthRejection::InvalidToken);

        // 401 文案不区分原因，也不回显提交值。
        for rejection in [missing, malformed, wrong] {
            assert_eq!(rejection.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(rejection.public_message(), wrong.public_message());
            assert!(!rejection.public_message().contains("not-the-token"));
        }
    }

    #[test]
    fn scheme_is_case_insensitive_but_token_is_not() {
        let token = format!("Tok-{}", Uuid::new_v4().simple());
        let secret = SecretToken::from_bytes("test", token.clone().into_bytes()).expect("secret");
        let authenticator = Authenticator {
            method: AuthMethod::Bearer,
            principal: mint_principal_id(),
            token: Some(secret),
        };
        let instance = mint_client_instance();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("bEaReR {token}").parse().expect("header"),
        );
        assert!(authenticator.authenticate(&headers, &instance).is_ok());
        assert!(!authenticator
            .authenticate(&bearer_headers(&token.to_lowercase()), &instance)
            .is_ok());
    }

    #[test]
    fn secret_token_debug_and_errors_never_contain_value() {
        let value = format!("sup3r-secret-{}", Uuid::new_v4().simple());
        let secret = SecretToken::from_bytes("test", value.clone().into_bytes()).expect("secret");
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "SecretToken(<redacted>)");
        assert!(!rendered.contains(&value));

        let empty = SecretToken::from_bytes("LOCAL_MCP_TOKEN", Vec::new()).expect_err("空值拒绝");
        let text = empty.to_string();
        assert!(!text.contains(&value), "错误文本不得包含 token 值: {text}");
        assert!(text.contains("empty"));
    }

    #[test]
    fn secret_token_from_file_trims_trailing_newline_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let value = format!("file-token-{}", Uuid::new_v4().simple());
        std::fs::write(&path, format!("{value}\n")).expect("write token file");
        let secret = SecretToken::from_file(&path, "token-file").expect("load");
        assert!(secret.verify(value.as_bytes()));
        assert!(!secret.verify(format!("{value}\n").as_bytes()));
    }

    #[test]
    fn secret_token_from_file_rejects_empty_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty-token");
        std::fs::write(&path, "   \n").expect("write");
        let err = SecretToken::from_file(&path, "token-file").expect_err("空文件必须拒绝");
        assert!(matches!(err, AuthError::EmptyToken { .. }));
    }

    #[test]
    fn secret_token_from_file_reports_missing_file_without_path_leak_of_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing");
        let err = SecretToken::from_file(&path, "token-file").expect_err("缺失文件必须拒绝");
        match err {
            AuthError::FileUnreadable { reason } => assert!(!reason.is_empty()),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn principal_from_parts_reads_extension_installed_by_transport() {
        let mut request = http::Request::new(());
        install_principal(
            request.extensions_mut(),
            AuthenticatedPrincipal::new("bearer-x", "http-conn-y", AuthMethod::Bearer),
        );
        let (parts, _) = request.into_parts();
        let principal = principal_from_parts(&parts).expect("extension 必须可见");
        assert_eq!(principal.principal(), "bearer-x");
        assert_eq!(principal.client_instance(), "http-conn-y");
        let context = principal.to_request_context("req-1");
        assert_eq!(context.principal, "bearer-x");
        assert_eq!(context.client_instance, "http-conn-y");
    }

    #[test]
    fn session_bindings_resolve_and_unbind() {
        let bindings = SessionBindings::new();
        assert!(bindings.is_empty());
        bindings.bind("session-1", "bearer-a", "http-conn-1");
        let binding = bindings.resolve("session-1").expect("绑定可见");
        assert_eq!(binding.principal, "bearer-a");
        assert_eq!(binding.client_instance, "http-conn-1");
        bindings.unbind("session-1");
        assert!(bindings.resolve("session-1").is_none());
        assert!(bindings.is_empty());
    }

    #[test]
    fn session_bindings_evict_oldest_beyond_capacity() {
        let bindings = SessionBindings::new();
        for index in 0..(MAX_BOUND_SESSIONS + 8) {
            let session = format!("session-{index}");
            bindings.bind(&session, "bearer-a", "http-conn-1");
        }
        assert_eq!(bindings.len(), MAX_BOUND_SESSIONS);
        assert!(bindings.resolve("session-0").is_none(), "最旧绑定应被淘汰");
        assert!(bindings
            .resolve(&format!("session-{}", MAX_BOUND_SESSIONS + 7))
            .is_some());
    }

    #[test]
    fn rebinding_same_session_keeps_single_entry() {
        let bindings = SessionBindings::new();
        bindings.bind("session-1", "bearer-a", "http-conn-1");
        bindings.bind("session-1", "bearer-a", "http-conn-1");
        assert_eq!(bindings.len(), 1);
    }
}
