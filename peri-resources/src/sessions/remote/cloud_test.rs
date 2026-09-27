//! 显式云端只读探测（默认 `#[ignore]`，不参与常规回归）。
//!
//! 本文件是凭证进入进程的唯一入口，只在本进程内解析操作者显式给出的 `.env`
//! （`PERI_CLOUD_ENV_FILE`，默认仓库根的 `.env`）：不 `source`/`eval`、不起子进程、
//! 不把变量注入其他进程环境、不修改 `.env`。
//!
//! 输出只允许：键名存在性、脱敏 engine/服务端特征、成功/失败分类。所有输出经 [`SafeOut`]
//! 逐行校验，凭证字面量一旦出现即拒绝输出。键名由操作者用 `PERI_CLOUD_URL_KEY` /
//! `PERI_CLOUD_TOKEN_KEY` 显式给出，本文件不内置默认名、别名或候选名。
//!
//! 两个探测都只发只读请求：不建表、不写数据、不初始化 schema。
//!
//! 显式执行（`--ignored`）时缺选择器或键**直接失败**，不静默跳过。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture cloud_
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceError, SessionResourceResult,
};
use peri_acp_types::store::MessageFlags;
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ProjectId, SessionBinding, WorkspaceId};
use turso_serverless::Value;

use super::ledger::{LedgerRow, OperationId, OperationIdentity};
use super::mutation::{MutationOutcome, QualifiedMutation, RemoteStore, StoreAccess};
use super::session_data::RemoteSessionData;
use super::sql::StatementSpec;
use super::{RemoteConnection, RemoteEndpoint, SessionStoreCredential};

/// URL 环境变量**键名**的选择器（不是凭证来源）。
pub(super) const URL_KEY_SELECTOR: &str = "PERI_CLOUD_URL_KEY";
/// 凭证环境变量**键名**的选择器。
pub(super) const TOKEN_KEY_SELECTOR: &str = "PERI_CLOUD_TOKEN_KEY";

/// 输出缓冲：打印前校验每行不含凭证字面量。
pub(super) struct SafeOut {
    lines: Vec<String>,
    secrets: Vec<String>,
}

impl SafeOut {
    pub(super) fn new(secrets: Vec<String>) -> Self {
        Self {
            lines: Vec::new(),
            secrets,
        }
    }

    pub(super) fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        for secret in &self.secrets {
            if secret.len() >= 8 && line.contains(secret.as_str()) {
                // 消息本身不含凭证内容。
                panic!("probe output rejected: line would contain a credential literal");
            }
        }
        self.lines.push(line);
    }

    pub(super) fn flush(&self) {
        for line in &self.lines {
            println!("PROBE {line}");
        }
    }
}

fn env_file_path() -> PathBuf {
    match std::env::var_os("PERI_CLOUD_ENV_FILE") {
        Some(path) => PathBuf::from(path),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"),
    }
}

/// 最小 dotenv 解析：只认 `KEY=VALUE`，剥一层对称引号；不做变量展开、不执行命令。
fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2 {
            let bytes = value.as_bytes();
            let quoted = (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
                || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'');
            if quoted {
                value = &value[1..value.len() - 1];
            }
        }
        entries.insert(key.to_owned(), value.to_owned());
    }
    entries
}

pub(super) fn load_env() -> BTreeMap<String, String> {
    let path = env_file_path();
    let text = std::fs::read_to_string(&path).expect("cloud probe env file unreadable");
    parse_env_file(&text)
}

/// 取操作者显式指定的两个键名对应的值。
///
/// 缺 selector 或缺键是**配置缺失**：显式执行时直接 panic，不静默跳过（跳过会被当成通过）。
pub(super) fn required_credential_keys() -> (String, String) {
    let url_key = std::env::var(URL_KEY_SELECTOR).unwrap_or_else(|_| {
        panic!("cloud experiment requires {URL_KEY_SELECTOR} (name of the locator variable)")
    });
    let token_key = std::env::var(TOKEN_KEY_SELECTOR).unwrap_or_else(|_| {
        panic!("cloud experiment requires {TOKEN_KEY_SELECTOR} (name of the token variable)")
    });
    (url_key, token_key)
}

pub(super) fn required_credentials(env: &BTreeMap<String, String>) -> (String, String) {
    let (url_key, token_key) = required_credential_keys();
    let missing = |key: &str| {
        panic!("cloud experiment requires a non-empty `{key}` entry in the env file");
    };
    let url = env
        .get(&url_key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| missing(&url_key))
        .clone();
    let token = env
        .get(&token_key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| missing(&token_key))
        .clone();
    (url, token)
}

pub(super) fn scheme_class(raw: &str) -> &'static str {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("https://") {
        "https"
    } else if lower.starts_with("turso://") {
        "turso"
    } else if lower.starts_with("libsql://") {
        "libsql"
    } else {
        "other"
    }
}

/// 官方 HTTP 参考页：`turso://` / `libsql://` 换成 `https://` 即同一 base URL。
pub(super) fn http_base_url(raw: &str) -> Result<url::Url, &'static str> {
    let rewritten = match scheme_class(raw) {
        "https" => raw.trim().to_owned(),
        "turso" => raw.trim().replacen("turso://", "https://", 1),
        "libsql" => raw.trim().replacen("libsql://", "https://", 1),
        _ => return Err("unsupported_scheme"),
    };
    let url = url::Url::parse(&rewritten).map_err(|_| "unparsable_url")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("userinfo_present");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("query_or_fragment_present");
    }
    Ok(url)
}

pub(super) fn classify_engine(body: &str) -> &'static str {
    let head = body.trim();
    if head.is_empty() {
        return "empty_body";
    }
    let lower = head.to_ascii_lowercase();
    if lower.starts_with("sqld") {
        "libsql_sqld"
    } else if lower.contains("turso") {
        "turso_engine"
    } else if lower.contains("libsql") {
        "libsql_engine_other"
    } else {
        "unclassified"
    }
}

/// 从任意文本里抽第一个 `x.y.z` 形态的数字版本；抽不到就报 `unrecognized`。
pub(super) fn numeric_version(body: &str) -> String {
    let mut found = String::new();
    let mut dots = 0;
    for byte in body.bytes() {
        let c = byte as char;
        if c.is_ascii_digit() {
            found.push(c);
        } else if c == '.' && !found.is_empty() && dots < 2 {
            found.push(c);
            dots += 1;
        } else {
            if dots == 2 && found.ends_with(|c: char| c.is_ascii_digit()) {
                return found;
            }
            found.clear();
            dots = 0;
        }
    }
    if dots == 2 && found.ends_with(|c: char| c.is_ascii_digit()) {
        found
    } else {
        "unrecognized".to_owned()
    }
}

pub(super) fn transport_class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_decode() {
        "decode"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    }
}

pub(super) fn unique_sentinel(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{prefix}-{nanos}")
}

/// 本轮唯一 run 标签：只用十六进制与短横线，供本轮对象命名（不作为身份判据）。
pub(super) fn unique_run_label(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{nanos:x}-{}", &nonce[..8])
}

/// 已授权测试库的目标：端点、凭证与脱敏字面量。
///
/// 故意不实现 `Debug`（结构里含 locator）；需要输出时只用 [`Self::out`]。
pub(super) struct CloudTarget {
    endpoint: RemoteEndpoint,
    credential: SessionStoreCredential,
    redactions: Vec<String>,
}

impl CloudTarget {
    /// 载入配置：缺文件、缺选择器、缺键、locator 形状不可用都直接 panic。
    pub(super) fn load() -> Self {
        let env = load_env();
        let (raw_url, token) = required_credentials(&env);
        let credential = SessionStoreCredential::new(token.clone()).unwrap_or_else(|error| {
            panic!("cloud experiment credential rejected: {error}");
        });
        let endpoint = RemoteEndpoint::parse(&raw_url, None).unwrap_or_else(|error| {
            panic!("cloud experiment locator rejected: {error}");
        });
        Self {
            endpoint,
            credential,
            redactions: vec![raw_url, token],
        }
    }

    /// 已解析的远端端点（只在本 crate 内用于装配实验；不打印原文）。
    pub(super) fn endpoint(&self) -> &RemoteEndpoint {
        &self.endpoint
    }

    /// 已解析的凭证值（装配实验直接注入，不经过进程环境）。
    pub(super) fn credential(&self) -> &SessionStoreCredential {
        &self.credential
    }

    pub(super) fn out(&self) -> SafeOut {
        SafeOut::new(self.redactions.clone())
    }

    /// 连接可变路径；连接失败即失败，不降级成「跳过」。
    pub(super) async fn store(&self, access: StoreAccess) -> SessionResourceResult<RemoteStore> {
        RemoteStore::connect(&self.endpoint, &self.credential, access).await
    }

    /// 连接会话数据 adapter（C-03 行为）：打开即读回 store 身份，未初始化时按访问模式
    /// 决定是否创建本任务 schema；失败即失败，不降级成「跳过」。
    ///
    /// 打开事实只有三个：端点、凭证、访问模式。本机不再持有远端操作的日志（v10 撤销了
    /// 「发送前登记、确定终态才结清」的本机记录），adapter 因此不需要任何本机库。
    pub(super) async fn session_data(
        &self,
        access: StoreAccess,
    ) -> SessionResourceResult<RemoteSessionData> {
        RemoteSessionData::open(&self.endpoint, &self.credential, access)
            .await
            .map(|(data, _initialization)| data)
    }
}

/// 未预期失败只给类别名（`SessionResourceError` 的 detail 已在该层脱敏）。
pub(super) fn remote_error_class(error: &SessionResourceError) -> String {
    format!("{:?}", error.kind())
}

// ─── 本轮 run 命名空间的共用夹具 ─────────────────────────────────────────────
//
// 所有显式云实验都只操作本轮 `run` 前缀下的行：结束前删除本轮**合成数据**（会话与消息）
// 并**复核计数为 0**（复核走新连接，见 [`cleanup_run`]）。语句是静态 SQL + 全绑定参数，
// `run` 只作为绑定值出现，不进入 SQL 文本。
//
// **收据不删**：`peri_op_ledger` 的每一行都是某次操作的封闭证据（终态 + 原收据），删除它
// 等于删掉「这件事发生过」——迟到的同 id 请求在证据不存在时无法被判为重复，而 P7 的口径
// 正是「收据不按 TTL 清理」。因此清理器里**没有**针对该表的删除/覆盖语句，收据是有意保留的
// 空间成本，复核时报告保留条数（[`check_cleanup_kept_receipts`]）。

pub(super) const DELETE_RUN_MESSAGES_SQL: &str = "DELETE FROM messages WHERE thread_id LIKE ?1";
pub(super) const DELETE_RUN_BINDINGS_SQL: &str =
    "DELETE FROM session_bindings WHERE thread_id LIKE ?1";
pub(super) const DELETE_RUN_SESSIONS_SQL: &str = "DELETE FROM threads WHERE id LIKE ?1";
pub(super) const COUNT_RUN_SESSIONS_SQL: &str = "SELECT COUNT(*) FROM threads WHERE id LIKE ?1";
pub(super) const COUNT_RUN_MESSAGES_SQL: &str =
    "SELECT COUNT(*) FROM messages WHERE thread_id LIKE ?1";
pub(super) const COUNT_RUN_BINDINGS_SQL: &str =
    "SELECT COUNT(*) FROM session_bindings WHERE thread_id LIKE ?1";
pub(super) const COUNT_RUN_LEDGER_SQL: &str =
    "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id LIKE ?1";

/// 清理器的效果语句：只删本轮合成数据，**不碰收据**。
///
/// 单独成函数是为了让「清理器不删收据」这条契约能在离线测试里被断言（见本文件的
/// `test_cleanup_never_deletes_ledger_receipts`），而不是只靠运行时的计数观察。
pub(super) fn cleanup_effects(run: &str) -> Vec<StatementSpec> {
    let prefix = Value::Text(format!("{run}%"));
    vec![
        StatementSpec::new(DELETE_RUN_MESSAGES_SQL, vec![prefix.clone()]),
        StatementSpec::new(DELETE_RUN_BINDINGS_SQL, vec![prefix.clone()]),
        StatementSpec::new(DELETE_RUN_SESSIONS_SQL, vec![prefix]),
    ]
}

/// 清理后的复核（纯函数，离线可测）。
///
/// 两件事都必须成立：本轮**合成**数据为 0；本轮**收据**只增不减（更新后的计数不小于清理前
/// 的计数，且清理自身那次操作也会留下一条）。返回有意保留的收据条数，由调用方报告。
pub(super) fn check_cleanup_kept_receipts(
    before: &RunCounts,
    after: &RunCounts,
) -> Result<i64, String> {
    check(
        after.sessions == 0 && after.messages == 0 && after.bindings == 0,
        "run namespace still has rows after cleanup",
    )?;
    check(
        before.ledger >= 0 && after.ledger >= before.ledger,
        "cleanup must not delete operation receipts",
    )?;
    Ok(after.ledger)
}

pub(super) fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

pub(super) fn failure(error: SessionResourceError) -> String {
    format!("unexpected failure: {}", remote_error_class(&error))
}

/// 合成一个绑定：远端只存绑定事实，不解析本机目录。
pub(super) fn synth_binding() -> SessionBinding {
    SessionBinding {
        schema_version: 1,
        revision: 1,
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd_relative_to_workspace: PathBuf::from("sub"),
    }
}

/// 合成一组 flags（实验里只关心 truncated/excluded）。
pub(super) fn synth_flags(truncated: bool, excluded: bool) -> MessageFlags {
    MessageFlags {
        truncated,
        excluded,
        projection: None,
    }
}

pub(super) fn synth_thread(label: &str) -> ThreadId {
    ThreadId::from(label)
}

/// 合成一条会话输入：全部内容由本轮 run 派生，不含真实历史或项目数据。
pub(super) fn session_input(
    thread_id: &str,
    created_at: &str,
    binding: &SessionBinding,
    frozen: &str,
    parent: Option<&str>,
) -> NewSession {
    NewSession {
        thread_id: thread_id.to_owned(),
        created_at: created_at.to_owned(),
        meta: NewSessionMeta {
            title: Some(format!(
                "title-{}",
                &thread_id[thread_id.len().saturating_sub(4)..]
            )),
            cwd: "/tmp/peri-cloud-synth".to_owned(),
            parent_thread_id: parent.map(str::to_owned),
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: binding.clone(),
        frozen: FrozenSnapshotBytes::new(frozen),
    }
}

/// 跑实验体、无论成败都清理本轮对象，最后一起判定。
pub(super) async fn with_cleanup<F, Fut>(
    target: &CloudTarget,
    run: &str,
    body: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let outcome = body().await;
    let cleanup = cleanup_run(target, run).await;
    match (outcome, cleanup) {
        (Err(message), _) => Err(message),
        (Ok(()), Err(message)) => Err(message),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// 删除本轮**合成**数据（会话与消息）并复核；**收据一律保留**。
///
/// 不能只凭「删除返回成功」就宣称清理完成，也不能把收据算进「本轮行」：收据是封闭证据，
/// 删除它会破坏「迟到请求不得被当成新操作」的判据（见本文件顶部与 P7）。
///
/// 复核走**新连接**（见 [`counts_after_cleanup`]）：收据「还在」只有在一次全新打开上仍可
/// 读回才算事实，同一连接上的读回可能只是本地视图。
pub(super) async fn cleanup_run(target: &CloudTarget, run: &str) -> Result<(), String> {
    let store = target
        .store(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let before = run_counts(&store, run).await?;
    let identity = OperationIdentity::new(
        OperationId::scoped(run, "session_cleanup"),
        "session_cleanup",
        &[run],
    );
    let outcome = store
        .apply_qualified(&QualifiedMutation {
            identity: identity.clone(),
            effects: cleanup_effects(run),
        })
        .await
        .map_err(failure)?;
    if !matches!(outcome, MutationOutcome::Applied { .. }) {
        return Err(format!("cleanup did not apply: {outcome:?}"));
    }
    store.close().await.map_err(failure)?;
    let after = counts_after_cleanup(target, run, &identity).await?;
    let retained = check_cleanup_kept_receipts(&before, &after)?;
    // 有意保留的收据条数（安全输出：只有数字）。
    let mut out = target.out();
    out.push(format!("retained_receipts={retained}"));
    out.flush();
    Ok(())
}

/// 清理之后在**新连接**上复核：本轮合成数据为 0，本轮收据仍在，且清理自身那张收据
/// 仍能经 adapter 读回（`Applied`）。
///
/// 这条路径同时覆盖两个方向：合成数据被真的删掉（不是只在本连接上消失），以及收据**没有**
/// 被清理语句顺手带走——收据只增不减，它的空间成本由 P7 口径承担。
async fn counts_after_cleanup(
    target: &CloudTarget,
    run: &str,
    cleanup: &OperationIdentity,
) -> Result<RunCounts, String> {
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    let receipt = store
        .resolve_operation(&cleanup.operation_id)
        .await
        .map_err(failure)?;
    store.close().await.map_err(failure)?;
    match receipt {
        LedgerRow::Applied { .. } => Ok(counts),
        other => Err(format!(
            "cleanup receipt must stay readable on a new connection: {other:?} (counts: sessions={} messages={} bindings={} ledger={})",
            counts.sessions, counts.messages, counts.bindings, counts.ledger
        )),
    }
}

pub(super) struct RunCounts {
    pub(super) sessions: i64,
    pub(super) messages: i64,
    pub(super) bindings: i64,
    pub(super) ledger: i64,
}

/// 本轮命名空间的计数（通过只读 SQL 复核，不经过 adapter 的读取行为）。
pub(super) async fn run_counts(store: &RemoteStore, run: &str) -> Result<RunCounts, String> {
    let batches = store
        .read_batch(vec![
            StatementSpec::new(COUNT_RUN_SESSIONS_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_MESSAGES_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_BINDINGS_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_LEDGER_SQL, vec![Value::Text(format!("%{run}%"))]),
        ])
        .await
        .map_err(failure)?;
    let mut counts = batches.into_iter().map(|mut rows| {
        rows.pop()
            .and_then(|mut row| row.pop())
            .and_then(|value| match value {
                Value::Integer(number) => Some(number),
                _ => None,
            })
            .unwrap_or(-1)
    });
    let (sessions, messages, bindings, ledger) = (
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
    );
    check(
        sessions >= 0 && messages >= 0 && bindings >= 0 && ledger >= 0,
        "run namespace counts are unreadable",
    )?;
    Ok(RunCounts {
        sessions,
        messages,
        bindings,
        ledger,
    })
}

/// 键名存在性报告：只列与引擎相关的键名，不输出任何取值。
#[test]
#[ignore = "显式 cloud 探测：需要已授权测试库的 .env，默认不跑"]
fn cloud_env_key_names_report() {
    let env = load_env();
    let candidates: Vec<&str> = env
        .keys()
        .filter(|key| {
            let upper = key.to_ascii_uppercase();
            upper.contains("TURSO") || upper.contains("LIBSQL")
        })
        .map(String::as_str)
        .collect();
    let mut out = SafeOut::new(env.values().cloned().collect());
    out.push(format!("env_keys_total={}", env.len()));
    out.push(format!("engine_like_key_names={candidates:?}"));
    for selector in [URL_KEY_SELECTOR, TOKEN_KEY_SELECTOR] {
        out.push(format!(
            "selector_present[{selector}]={}",
            std::env::var_os(selector).is_some()
        ));
    }
    out.flush();
}

/// 协议层只读探测：`GET /version` 与服务端特征 + `POST /v2/pipeline` 只读参数绑定回环。
#[tokio::test]
#[ignore = "显式 cloud 探测：需要已授权测试库的 .env，默认不跑"]
async fn cloud_engine_read_only_probe() {
    let env = load_env();
    let (raw_url, token) = required_credentials(&env);
    let mut out = SafeOut::new(vec![raw_url.clone(), token.clone()]);
    out.push(format!("url_scheme_class={}", scheme_class(&raw_url)));

    let base = match http_base_url(&raw_url) {
        Ok(url) => url,
        Err(class) => {
            out.push(format!("url_shape={class}"));
            out.flush();
            return;
        }
    };
    out.push("url_shape=ok".to_owned());
    out.push(format!("token_length_class={}", token.len() / 16));

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            out.push(format!("client_build={}", transport_class(&error)));
            out.flush();
            return;
        }
    };

    match base.join("version") {
        Ok(endpoint) => match client.get(endpoint).bearer_auth(&token).send().await {
            Ok(response) => {
                out.push(format!(
                    "version_http_status={}",
                    response.status().as_u16()
                ));
                let body = response.text().await.unwrap_or_default();
                out.push(format!("engine_class={}", classify_engine(&body)));
                out.push(format!("server_version_numeric={}", numeric_version(&body)));
            }
            Err(error) => out.push(format!("version_transport={}", transport_class(&error))),
        },
        Err(_) => out.push("version_endpoint=join_failed".to_owned()),
    }

    let sentinel = unique_sentinel("peri-probe");
    let request = serde_json::json!({
        "requests": [
            {
                "type": "execute",
                "stmt": {
                    "sql": "SELECT ? AS probe_text, ? AS probe_int, ? AS probe_null",
                    "args": [
                        {"type": "text", "value": sentinel},
                        {"type": "integer", "value": "9007199254740993"},
                        {"type": "null"}
                    ]
                }
            },
            {"type": "close"}
        ]
    });
    match base.join("v2/pipeline") {
        Ok(endpoint) => match client
            .post(endpoint)
            .bearer_auth(&token)
            .json(&request)
            .send()
            .await
        {
            Ok(response) => {
                out.push(format!(
                    "pipeline_http_status={}",
                    response.status().as_u16()
                ));
                let body: serde_json::Value =
                    response.json().await.unwrap_or(serde_json::Value::Null);
                let entry = &body["results"][0];
                let kind = entry["type"].as_str().unwrap_or("malformed");
                out.push(format!("pipeline_result_class={kind}"));
                if kind == "ok" {
                    let row = &entry["response"]["result"]["rows"][0];
                    out.push(format!(
                        "binding_text_roundtrip={}",
                        row[0]["value"].as_str() == Some(sentinel.as_str())
                    ));
                    out.push(format!(
                        "binding_int64_roundtrip={}",
                        row[1]["value"].as_str() == Some("9007199254740993")
                    ));
                    out.push(format!(
                        "binding_null_roundtrip={}",
                        row[2]["type"].as_str() == Some("null") || row[2].is_null()
                    ));
                } else if kind == "error" {
                    let code = entry["error"]["code"]
                        .as_str()
                        .filter(|code| {
                            code.len() <= 40
                                && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        })
                        .unwrap_or("unclassified");
                    out.push(format!("pipeline_error_code={code}"));
                }
            }
            Err(error) => out.push(format!("pipeline_transport={}", transport_class(&error))),
        },
        Err(_) => out.push("pipeline_endpoint=join_failed".to_owned()),
    }

    out.flush();
}

/// 生产连接路径只读探测：用私有 `RemoteConnection`（已选定 SDK）连接并做只读回环。
#[tokio::test]
#[ignore = "显式 cloud 探测：需要已授权测试库的 .env，默认不跑"]
async fn cloud_sdk_connection_read_only_probe() {
    let env = load_env();
    let (raw_url, token) = required_credentials(&env);
    let credential = match SessionStoreCredential::new(token.clone()) {
        Ok(credential) => credential,
        Err(error) => {
            println!("PROBE credential_error={error}");
            return;
        }
    };
    let mut out = SafeOut::new(vec![raw_url.clone(), token.clone()]);
    let endpoint = match RemoteEndpoint::parse(&raw_url, None) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            out.push(format!("endpoint_error={error}"));
            out.flush();
            return;
        }
    };
    out.push(format!("engine={}", endpoint.engine().as_str()));
    out.push(format!("host_domain_class={}", endpoint.host_class()));
    out.push(format!("token_length_class={}", credential.length_class()));

    let connection = match RemoteConnection::connect(&endpoint, &credential).await {
        Ok(connection) => connection,
        Err(error) => {
            out.push(format!("sdk_connect_error_kind={:?}", error.kind()));
            out.flush();
            return;
        }
    };
    out.push("sdk_connect=ok".to_owned());

    let sentinel = unique_sentinel("peri-sdk-probe");
    match connection
        .bind_roundtrip(&sentinel, 9_007_199_254_740_993)
        .await
    {
        Ok(roundtrip) => {
            out.push(format!("sdk_binding_text_ok={}", roundtrip.text_ok));
            out.push(format!("sdk_binding_int64_ok={}", roundtrip.int64_ok));
            out.push(format!("sdk_binding_null_ok={}", roundtrip.null_ok));
        }
        Err(error) => out.push(format!("sdk_binding_error_kind={:?}", error.kind())),
    }

    match connection.engine_read_facts().await {
        Ok(facts) => out.push(format!(
            "sdk_sqlite_version_numeric={}",
            facts
                .sqlite_version
                .as_deref()
                .map(numeric_version)
                .unwrap_or_else(|| "unsupported".to_owned())
        )),
        Err(error) => out.push(format!("sdk_read_facts_error_kind={:?}", error.kind())),
    }

    match connection.close().await {
        Ok(()) => out.push("sdk_close=ok".to_owned()),
        Err(error) => out.push(format!("sdk_close_error_kind={:?}", error.kind())),
    }
    out.flush();
}

// ─── 清理器的离线契约测试（不联网、不需要凭证）────────────────────────────────

/// 清理器只删本轮**合成**数据：效果语句里不得出现收据表，也不得把 run 前缀编进 SQL 文本。
#[test]
fn test_cleanup_never_deletes_ledger_receipts() {
    let run = "peri-run-0123456789";
    let effects = cleanup_effects(run);
    assert_eq!(
        effects.len(),
        3,
        "清理本轮合成数据的三条语句（子行先于父行）"
    );
    for spec in &effects {
        assert!(
            !spec.sql.contains("peri_op_ledger"),
            "清理器不得删除收据行：{}",
            spec.sql
        );
        assert!(
            spec.sql.starts_with("DELETE FROM threads")
                || spec.sql.starts_with("DELETE FROM messages")
                || spec.sql.starts_with("DELETE FROM session_bindings"),
            "清理器只删本轮合成的会话与消息：{}",
            spec.sql
        );
        assert!(
            !spec.sql.contains(run),
            "run 只作为绑定值出现，不进入 SQL 文本：{}",
            spec.sql
        );
        assert!(
            spec.params
                .iter()
                .any(|value| matches!(value, Value::Text(text) if text == &format!("{run}%"))),
            "本轮前缀必须是绑定参数"
        );
    }
}

/// 清理后的复核：合成数据必须为 0，收据只增不减，返回值是**有意保留**的条数。
#[test]
fn test_cleanup_retention_check_reports_retained_receipts() {
    let before = RunCounts {
        sessions: 1,
        messages: 3,
        bindings: 1,
        ledger: 2,
    };
    // 清理自身这次的收据也会落在账本里：保留条数因此可以增加，但绝不减少。
    let after = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 0,
        ledger: 3,
    };
    assert_eq!(check_cleanup_kept_receipts(&before, &after).unwrap(), 3);

    let lost = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 0,
        ledger: 1,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &lost).is_err(),
        "收据被删掉必须失败，不静默通过"
    );

    let dirty = RunCounts {
        sessions: 1,
        messages: 0,
        bindings: 0,
        ledger: 3,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &dirty).is_err(),
        "合成数据没删干净必须失败"
    );

    let orphan_binding = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 1,
        ledger: 3,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &orphan_binding).is_err(),
        "绑定行没删干净必须失败"
    );
}
