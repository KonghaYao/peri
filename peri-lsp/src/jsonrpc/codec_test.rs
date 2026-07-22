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
