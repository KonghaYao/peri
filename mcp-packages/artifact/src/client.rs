use thiserror::Error;

const DEFAULT_URL: &str = "https://cloud-artifacts.claude-code-best.win";
// 免费公共服务使用的共享协议标识，不是用户凭据或 secret；不得写入日志。
const DEFAULT_TOKEN: &str = "claude-code-best";

/// 错误文本里保留的响应体上限：避免超大 body 淹没模型可见结果。
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;

/// 上传失败的结构化错误。
///
/// 实现 `std::error::Error + Send + Sync`，`ArtifactTool::invoke` 可直接 `?` 传播，
/// 由 MCP 映射为 `isError=true`（`peri_mcp_common::invoke_tool_call`）。
#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("Upload failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Failed to read response: {source}")]
    ReadResponse { source: reqwest::Error },
    #[error("HTTP {status}: {body}")]
    HttpStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("Failed to parse response: {source}. Body: {body}")]
    Parse {
        source: serde_json::Error,
        body: String,
    },
    #[error("Upload error: {error}")]
    Server { error: String },
    #[error("Artifact response has no usable URL")]
    MissingUrl,
    #[error("Artifact response URL is invalid: {url}")]
    InvalidUrl { url: String },
}

/// CCB Artifacts 服务 HTTP 客户端。
pub struct ArtifactClient {
    base_url: String,
    token: String,
}

/// 服务端响应（兼容 Deno Deploy 抹平 HTTP 状态码的情况，错误字段在 body 中）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArtifactResponse {
    #[serde(default)]
    #[allow(dead_code)] // 服务响应兼容字段；当前输出不展示上传 ID。
    pub id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, alias = "expiresAt")]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ArtifactClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self { base_url, token }
    }

    /// 只解释传给本 MCP 实例的环境，不读取宿主进程环境。
    pub fn from_instance_env(env: &std::collections::HashMap<String, String>) -> Self {
        let url = env
            .get("PERI_ARTIFACTS_URL")
            .cloned()
            .unwrap_or_else(|| DEFAULT_URL.into());
        let token = env
            .get("PERI_ARTIFACTS_TOKEN")
            .cloned()
            .unwrap_or_else(|| DEFAULT_TOKEN.into());
        Self::new(url, token)
    }

    pub fn upload_url(&self) -> String {
        format!("{}/upload", self.base_url.trim_end_matches('/'))
    }

    /// 上传 HTML 文件内容并返回服务端响应（调用方经 [`Self::format_output`] 渲染）。
    ///
    /// 只有 2xx 才进入成功解析；非 2xx、2xx 带 `error`、响应不可解析、URL 缺失或非法
    /// 都是 [`ArtifactError`]，由工具层映射为 `isError=true`。
    pub async fn upload(
        &self,
        content: &str,
        ttl: &str,
    ) -> Result<ArtifactResponse, ArtifactError> {
        let url = self.upload_url();

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "text/html")
            // Deno Deploy 可能返回无法被 reqwest 正确解码的 Brotli body；
            // 响应 JSON 很小，禁用压缩可避免读取响应失败。
            .header("Accept-Encoding", "identity")
            .header("X-TTL", ttl)
            .body(content.to_string())
            .send()
            .await
            .map_err(ArtifactError::Transport)?;

        let status = response.status();
        let resp_text = response
            .text()
            .await
            .map_err(|source| ArtifactError::ReadResponse { source })?;

        Self::interpret_upload_response(status, &resp_text)
    }

    /// 把 HTTP 状态码与响应体解释为成功响应或结构化错误（纯函数，便于覆盖边界）。
    fn interpret_upload_response(
        status: reqwest::StatusCode,
        body: &str,
    ) -> Result<ArtifactResponse, ArtifactError> {
        if !status.is_success() {
            // 服务端在 JSON 里给出 error 时优先保留该诊断，否则回退到截断后的原文。
            let detail = serde_json::from_str::<ArtifactResponse>(body)
                .ok()
                .and_then(|parsed| parsed.error)
                .unwrap_or_else(|| body.to_string());
            return Err(ArtifactError::HttpStatus {
                status,
                body: truncate_error_body(detail.trim()),
            });
        }

        let mut parsed: ArtifactResponse =
            serde_json::from_str(body).map_err(|source| ArtifactError::Parse {
                source,
                body: truncate_error_body(body),
            })?;

        if let Some(error) = parsed.error {
            return Err(ArtifactError::Server {
                error: truncate_error_body(error.trim()),
            });
        }

        let url = parsed.url.trim();
        validate_artifact_url(url)?;
        parsed.url = url.to_string();
        Ok(parsed)
    }

    /// 将成功响应格式化为纯文本 URL（不含 OSC 8 转义序列）。
    ///
    /// **历史踩坑**：早期版本把 URL 包裹成 OSC 8 超链接 escape 序列后作为
    /// tool_result 返回给 LLM。LLM 不识别终端控制序列，把 `\x1b` 当作字面
    /// 字符回显到回答中，TUI 把 ESC 字符渲染成 `␛` 符号，用户看到
    /// `␛]8;;URL␛\URL␛]8;;␛\` 这样的乱码。
    /// OSC 8 应该在 UI 渲染层（如 `LinkSpan` / Markdown link 渲染器）处理，
    /// tool_result 只返回纯 URL 文本。
    pub fn format_output(resp: &ArtifactResponse) -> String {
        let mut output = format!("Artifact uploaded: {}\n", resp.url);
        if let Some(ref expires) = resp.expires_at {
            output.push_str(&format!("Expires: {}", expires));
        }
        output
    }
}

impl Default for ArtifactClient {
    fn default() -> Self {
        Self::new(DEFAULT_URL.to_string(), DEFAULT_TOKEN.to_string())
    }
}

/// 截断错误响应体（按 UTF-8 字符边界），并标注原始大小。
fn truncate_error_body(body: &str) -> String {
    if body.len() <= MAX_ERROR_BODY_BYTES {
        return body.to_string();
    }
    let mut end = MAX_ERROR_BODY_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated, {} bytes total)", &body[..end], body.len())
}

/// 只接受非空绝对 HTTP(S) URL：相对路径与自定义 scheme 都不能作为公开链接。
fn validate_artifact_url(url: &str) -> Result<(), ArtifactError> {
    if url.is_empty() {
        return Err(ArtifactError::MissingUrl);
    }
    match reqwest::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => Ok(()),
        _ => Err(ArtifactError::InvalidUrl {
            url: truncate_error_body(url),
        }),
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
