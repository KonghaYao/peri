use tokio::io::{BufReader, BufWriter};

use super::*;

#[tokio::test]
async fn test_encode_decode_roundtrip() {
    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let mut buf = Vec::new();
    let mut writer = BufWriter::new(&mut buf);
    encode_message(msg.as_bytes(), &mut writer).await.unwrap();

    let mut reader = BufReader::new(&buf[..]);
    let decoded = decode_message(&mut reader).await.unwrap();
    assert_eq!(decoded.as_deref(), Some(msg));
}

#[tokio::test]
async fn test_encode_decode_multiple_messages() {
    let msg1 = r#"{"jsonrpc":"2.0","id":1,"method":"init"}"#;
    let msg2 = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#;
    let mut buf = Vec::new();
    let mut writer = BufWriter::new(&mut buf);
    encode_message(msg1.as_bytes(), &mut writer).await.unwrap();
    encode_message(msg2.as_bytes(), &mut writer).await.unwrap();

    let mut reader = BufReader::new(&buf[..]);
    assert_eq!(
        decode_message(&mut reader).await.unwrap().as_deref(),
        Some(msg1)
    );
    assert_eq!(
        decode_message(&mut reader).await.unwrap().as_deref(),
        Some(msg2)
    );
    assert!(decode_message(&mut reader).await.unwrap().is_none());
}

#[tokio::test]
async fn test_decode_eof() {
    let buf: &[u8] = b"";
    let mut reader = BufReader::new(buf);
    assert!(decode_message(&mut reader).await.unwrap().is_none());
}

#[tokio::test]
async fn test_decode_lowercase_content_length_header() {
    // 小写 content-length 头（RFC 7230 头部字段名大小写不敏感）不应丢帧
    let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
    let buf = format!("content-length: {}\r\n\r\n{}", body.len(), body).into_bytes();
    let mut reader = BufReader::new(&buf[..]);
    let decoded = decode_message(&mut reader).await.unwrap();
    assert_eq!(decoded.as_deref(), Some(body));
}

#[tokio::test]
async fn test_decode_mixed_case_content_length_with_other_headers() {
    // 混合大小写 + 其他头部行（如 Content-Type）时仍应正确分帧
    let body = r#"{"jsonrpc":"2.0","id":2,"result":null}"#;
    let buf = format!(
        "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncOnTeNt-LeNgTh: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes();
    let mut reader = BufReader::new(&buf[..]);
    let decoded = decode_message(&mut reader).await.unwrap();
    assert_eq!(decoded.as_deref(), Some(body));
}

#[tokio::test]
async fn test_decode_rejects_oversized_content_length() {
    // 超上限声明：直接报 -32700，而非按声明长度分配（64MB+）再等 EOF
    let buf = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1).into_bytes();
    let mut reader = BufReader::new(&buf[..]);
    let err = decode_message(&mut reader).await.unwrap_err();
    assert!(
        matches!(err, LspError::JsonRpcError { code: -32700, .. }),
        "超限应返回 -32700 错误: {err:?}"
    );
}
