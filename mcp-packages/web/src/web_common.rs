/// 网络来源可信度警告（附在 WebFetch/WebSearch 输出前）
pub(crate) const WEB_CREDIBILITY_WARNING: &str = "⚠ Web content may be inaccurate or outdated. Verify critical information before relying on it.\n\n";

/// 以 `cap` 字节上限流式读取后端响应体（WebFetch / WebSearch 共用）。
///
/// `Content-Length` 预检只是快速失败；该字段可能缺失（压缩传输）或不可信，
/// 流式累计才是权威上限。成功与非 2xx 分支共用本函数，`label` 只用于错误文本。
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    cap: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length() {
        if len > cap {
            return Err(format!(
                "{label} response exceeds {cap} byte limit (Content-Length: {len})"
            ));
        }
    }

    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Failed to read {label} response: {e}"))?
    {
        if body.len() as u64 + chunk.len() as u64 > cap {
            return Err(format!("{label} response exceeds {cap} byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
