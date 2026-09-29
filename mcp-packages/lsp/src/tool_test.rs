//! LspTool 的 file_to_uri 路径 → URI 转换测试
//! 以及查询前 didOpen 行为测试（perl fake LSP server 全链路）

use std::{collections::HashMap, sync::Arc};

use super::*;
use peri_agent::tools::ToolContext;
use peri_resources::lsp::config::{LspConfigFile, LspServerConfig};
use peri_resources::lsp::pool::LspServerPool;

/// perl 编写的极简 LSP 服务器（同 peri-lsp client_test.rs）：
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行 "spawned"
/// - didOpen 通知的完整 JSON body 追加到 `$PERI_LSP_TEST_DIDOPEN`
/// - 对任何带 id 的请求回 `{"result":null}`（满足 initialize 握手与查询请求）
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"method"\s*:\s*"textDocument\/didOpen"/) {
        open my $f, '>>', $ENV{PERI_LSP_TEST_DIDOPEN} or next;
        print $f "$b\n";
        close $f;
    }
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造以 perl fake server 为后端的 LspTool（.rs 扩展名路由），
/// 返回 (tool, didOpen 记录文件路径)
fn make_fake_tool(dir: &std::path::Path) -> (LspTool, std::path::PathBuf) {
    let didopen_file = dir.join("didopen.txt");
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        dir.join("spawn_count.txt").to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_DIDOPEN".to_string(),
        didopen_file.to_string_lossy().into_owned(),
    );
    let config = LspConfigFile {
        lsp_servers: HashMap::from([(
            "fake-lsp".to_string(),
            LspServerConfig {
                name: "fake-lsp".to_string(),
                command: "perl".to_string(),
                args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
                env: Some(env),
                extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
                initialization_options: None,
                disabled: None,
                max_restarts: Some(3),
                startup_timeout: None,
                source: None,
            },
        )]),
    };
    let pool = Arc::new(LspServerPool::new(dir.to_str().unwrap(), config));
    (LspTool::new(pool), didopen_file)
}

/// 统计 fake server 收到的 didOpen 通知次数
fn didopen_count(didopen_file: &std::path::Path) -> usize {
    std::fs::read_to_string(didopen_file)
        .map(|s| s.matches("textDocument/didOpen").count())
        .unwrap_or(0)
}

/// 查询 documentSymbol 并断言查询本身成功
async fn query_document_symbol(tool: &LspTool, file_path: &str) {
    let result = tool
        .invoke(
            serde_json::json!({"operation": "documentSymbol", "file_path": file_path}),
            ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        result.is_ok(),
        "documentSymbol 查询失败: {:?}",
        result.err()
    );
}

#[test]
fn test_file_to_uri_absolute_with_space_and_chinese() {
    // 空格与中文按 RFC 3986 percent-encode，保留 `/` 分隔符。
    // Windows 上 /tmp 落到当前盘根（file:///D:/tmp/...），断言公共部分
    let uri = LspTool::file_to_uri("/tmp/my dir/源码.rs");
    assert!(uri.starts_with("file:///"), "got {uri}");
    assert!(uri.contains("/tmp/my%20dir/"), "got {uri}");
    assert!(uri.contains("%E6%BA%90%E7%A0%81"), "got {uri}");
    assert!(!uri.contains(' '), "URI 不应包含未编码空格: {uri}");
}

#[test]
fn test_file_to_uri_relative_absolutized() {
    // 相对路径基于当前工作目录绝对化
    let uri = LspTool::file_to_uri("src/main.rs");
    assert!(uri.starts_with("file://"), "got {uri}");
    assert!(uri.ends_with("/src/main.rs"), "got {uri}");
    assert!(!uri.starts_with("file://src"), "相对路径未绝对化: {uri}");
}

#[test]
fn test_file_to_uri_already_uri_idempotent() {
    // 已带 file:// 前缀的输入原样返回，不产生 file://file://
    let uri = "file:///tmp/main.rs";
    assert_eq!(LspTool::file_to_uri(uri), uri);
}

#[test]
fn test_file_to_uri_already_uri_no_double_prefix() {
    // 传入完整 URI 不应再叠加 file:// 前缀
    let uri = LspTool::file_to_uri("file:///tmp/my%20dir/源码.rs");
    assert_eq!(uri, "file:///tmp/my%20dir/源码.rs");
    assert!(!uri.starts_with("file://file://"), "双重前缀残留: {uri}");
}

#[tokio::test]
async fn test_first_query_triggers_did_open_with_content() {
    // 首次查询应触发一次 didOpen，携带文件内容、按扩展名推断的 languageId 与 uri
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("main.rs");
    std::fs::write(&src, "fn main() {}\n").unwrap();
    let (tool, didopen_file) = make_fake_tool(dir.path());
    let path = src.to_string_lossy().to_string();

    query_document_symbol(&tool, &path).await;

    assert_eq!(
        didopen_count(&didopen_file),
        1,
        "首次查询应触发一次 didOpen"
    );
    let record = std::fs::read_to_string(&didopen_file).unwrap();
    assert!(
        record.contains("fn main() {}"),
        "didOpen 应携带文件内容: {record}"
    );
    assert!(
        record.contains("\"languageId\":\"rust\""),
        "languageId 应按扩展名推断: {record}"
    );
    // didOpen 的 uri 应包含文件路径（Windows 上 URI 为正斜杠形式，
    // 原路径为反斜杠，统一用正斜杠比对）
    let uri_path = path.replace('\\', "/");
    assert!(
        record.contains(&uri_path),
        "didOpen uri 应为文件路径: {record}"
    );
}

#[tokio::test]
async fn test_repeated_query_does_not_repeat_did_open() {
    // 已打开的文件再次查询不应重复发送 didOpen（client 侧缓存幂等）
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("main.rs");
    std::fs::write(&src, "fn main() {}\n").unwrap();
    let (tool, didopen_file) = make_fake_tool(dir.path());
    let path = src.to_string_lossy().to_string();

    for _ in 0..2 {
        query_document_symbol(&tool, &path).await;
    }

    assert_eq!(
        didopen_count(&didopen_file),
        1,
        "重复查询不应再次发送 didOpen"
    );
}

#[tokio::test]
async fn test_missing_file_skips_did_open_without_blocking() {
    // 文件不存在时读取失败应跳过 didOpen，且不阻塞查询本身
    let dir = tempfile::tempdir().unwrap();
    let (tool, didopen_file) = make_fake_tool(dir.path());
    let missing = dir.path().join("missing.rs");

    query_document_symbol(&tool, &missing.to_string_lossy()).await;

    assert_eq!(
        didopen_count(&didopen_file),
        0,
        "文件不存在不应发送 didOpen"
    );
}
