//! 日志出口、脱敏与审计（WP-007 集成实现）。
//!
//! 本模块是**唯一**的诊断出口，产品其余部分只调用这里的函数，不自行 `println!`、
//! 不自行安装 tracing subscriber：
//!
//! | 出口 | 用途 | 去向 |
//! | --- | --- | --- |
//! | [`init_diagnostics`] | 两个 bin 的 tracing 初始化 | **stderr**（stdio 下 stdout 属于协议通道） |
//! | [`audit_tool_call`] | 逐次工具调用的审计行 | stderr（结构化字段） |
//! | [`redact_for_log`] | 不受控文本写日志前的凭证脱敏 | 调用方（stderr 前） |
//!
//! ## 为什么参数不进入审计行
//!
//! 工具参数既可能是用户源码，也可能是命令里的凭证（`export TOKEN=…`）。审计只记录
//! 「谁、调用了哪个工具、结果如何、耗时多少」——这些足以支撑审计，且**结构上**不可能
//! 把参数里的秘密写进日志。这与 HTTP 传输层"不记录 Authorization"的规则互补。
//!
//! ## 脱敏是纵深防御，不是靠调用方自律
//!
//! 工具输出与被转储的诊断文本不受本产品控制，因此写入日志前统一过 [`redact_for_log`]，
//! 覆盖两类高熵凭证形态：`Authorization: …`/`Bearer …` 头与长随机串
//! （≥32 位十六进制或 ≥40 位 base64 形态）。

use std::time::Duration;

/// 初始化诊断输出：**始终写 stderr**，级别由 `RUST_LOG` 控制。
///
/// stdio 传输下 stdout 属于协议通道：只能经这里初始化诊断，`println!` 会直接污染协议。
pub fn init_diagnostics() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .init();
}

/// 一次工具调用的审计事实（**不含任何参数**）。
#[derive(Debug, Clone, Copy)]
pub struct ToolAudit<'a> {
    /// 协议层请求 id（用于把审计行与 wire 上的请求对应起来）。
    pub request_id: &'a str,
    /// 可信主体（HTTP bearer 主体是进程内随机 id，与 token 值无派生关系）。
    pub principal: &'a str,
    /// 连接实例（每条可信连接唯一）。
    pub client_instance: &'a str,
    /// 规范工具名。
    pub tool: &'a str,
    /// 调用是否成功（业务失败也算"已服务"，用 `outcome` 区分）。
    pub outcome: ToolOutcome,
    /// 端到端耗时。
    pub elapsed: Duration,
}

/// 工具调用的审计结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    /// 工具返回了非错误结果。
    Ok,
    /// 工具返回了 `isError=true` 的业务失败（输入校验、文件不存在、命令非零退出…）。
    ToolError,
    /// 协议级失败：未知工具、形状错误、后端不可用。
    ProtocolError,
}

impl ToolOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::ToolError => "tool_error",
            Self::ProtocolError => "protocol_error",
        }
    }
}

/// 写一行审计。字段里只有不透明 id 与工具名，没有参数、没有凭证。
pub fn audit_tool_call(audit: &ToolAudit<'_>) {
    tracing::info!(
        target: "local_mcp_server::audit",
        request_id = audit.request_id,
        principal = audit.principal,
        client_instance = audit.client_instance,
        tool = audit.tool,
        outcome = audit.outcome.as_str(),
        elapsed_ms = audit.elapsed.as_millis() as u64,
        "tool call"
    );
}

/// 对一段文本做凭证脱敏（用于把不受控文本写进日志之前）。
///
/// 覆盖：
/// - `Authorization: …` / `authorization=…`（含无空格形态）之后的值；
/// - 独立词 `Bearer` 之后的 token；
/// - ≥32 位十六进制串（256 位句柄、session id 一类）；
/// - ≥40 位、同时含字母与数字的连续高熵串（凭证常见形态）。
///
/// 不做"猜测式"通配删除：普通单词、路径、数字常量必须原样保留，否则日志失去诊断价值。
pub fn redact_for_log(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // 两个"待脱敏"标记：`authorization` 之后的值、`bearer` 之后的 token。
    let mut pending_authorization = false;
    let mut pending_bearer = false;

    for chunk in split_keeping_whitespace(text) {
        if chunk.chars().all(char::is_whitespace) {
            out.push_str(chunk);
            continue;
        }
        let word = chunk.trim_end_matches([',', ';', '"']);
        let lowered = word.to_ascii_lowercase();

        if pending_authorization {
            pending_authorization = false;
            if lowered == "bearer" {
                // 保留方案名，脱敏它后面的 token。
                out.push_str(chunk);
                pending_bearer = true;
            } else {
                out.push_str("<redacted>");
            }
            continue;
        }
        if pending_bearer {
            pending_bearer = false;
            out.push_str("<redacted>");
            continue;
        }

        if let Some(value) = inline_header_value(word, "authorization") {
            out.push_str(&word[..word.len() - value.len()]);
            out.push_str("<redacted>");
            continue;
        }
        if lowered == "authorization" {
            out.push_str(chunk);
            pending_authorization = true;
            continue;
        }
        if lowered == "bearer" {
            out.push_str(chunk);
            pending_bearer = true;
            continue;
        }
        out.push_str(chunk);
    }

    redact_high_entropy(&out)
}

/// 按空白切分但保留空白片段（缩进与换行必须留在日志里）。
fn split_keeping_whitespace(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut in_space = None::<bool>;
    for (index, ch) in text.char_indices() {
        let is_space = ch.is_whitespace();
        match in_space {
            None => in_space = Some(is_space),
            Some(current) if current != is_space => {
                chunks.push(&text[start..index]);
                start = index;
                in_space = Some(is_space);
            }
            _ => {}
        }
    }
    if start < text.len() {
        chunks.push(&text[start..]);
    }
    chunks
}

/// `Authorization:value`（无空格）形态：返回 `:value` 这一段（含分隔符）。
fn inline_header_value<'a>(word: &'a str, key: &str) -> Option<&'a str> {
    // 必须以 `char_boundary` 判定：诊断文本常含中文，按字节切会 panic。
    if word.len() <= key.len()
        || !word.is_char_boundary(key.len())
        || !word[..key.len()].eq_ignore_ascii_case(key)
    {
        return None;
    }
    let separator = word[key.len()..].chars().next()?;
    if separator != ':' && separator != '=' {
        return None;
    }
    let mut value_start = key.len() + separator.len_utf8();
    while let Some(ch) = word[value_start..].chars().next() {
        if ch == ':' || ch == '=' || ch.is_whitespace() {
            value_start += ch.len_utf8();
        } else {
            break;
        }
    }
    Some(&word[value_start..])
}

/// 把长十六进制串与长高熵串替换为固定占位符。
fn redact_high_entropy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let start = index;
        let mut has_digit = false;
        let mut has_letter = false;
        let mut len = 0usize;
        while index < bytes.len() {
            let byte = bytes[index];
            let is_word =
                byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'=';
            if !is_word {
                break;
            }
            if byte.is_ascii_digit() {
                has_digit = true;
            }
            if byte.is_ascii_alphabetic() {
                has_letter = true;
            }
            len += 1;
            index += 1;
        }
        if len == 0 {
            // 非字词字符：原样输出（按字符边界推进）。
            let ch = text[start..].chars().next().expect("char boundary");
            out.push(ch);
            index = start + ch.len_utf8();
            continue;
        }
        let token = &text[start..index];
        // 十六进制形态的句柄/摘要：全 hex 且足够长即视为高熵。
        let hex_like = token.bytes().all(|byte| byte.is_ascii_hexdigit()) && len >= 32;
        // 混合形态：字母 + 数字且足够长（避免误伤 40 位以上的普通单词是不可能的，
        // 因为普通标识符不会同时含字母与数字且长达 40 位）。
        let mixed_like = has_digit && has_letter && len >= 40;
        if hex_like || mixed_like {
            out.push_str("<redacted:high-entropy>");
        } else {
            out.push_str(token);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redaction_removes_authorization_values_and_bearer_tokens() {
        let text = "GET / HTTP/1.1 Authorization: Bearer abc123SECRET value\nnext";
        let redacted = redact_for_log(text);
        assert!(!redacted.contains("abc123SECRET"), "{redacted}");
        assert!(redacted.contains("<redacted>") || redacted.contains("<redacted:high-entropy>"));
        assert!(redacted.contains("next"), "非敏感文本必须保留");
    }

    #[test]
    fn test_redaction_removes_long_hex_handles_but_keeps_normal_words() {
        let handle = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let text = format!("log handle {handle} written to /tmp/local-mcp-ws/notes/readme.md");
        let redacted = redact_for_log(&text);
        assert!(!redacted.contains(handle));
        assert!(redacted.contains("/tmp/local-mcp-ws/notes/readme.md"));
        assert!(redacted.contains("readme.md"));
    }

    #[test]
    fn test_redaction_handles_multibyte_diagnostics_without_panicking() {
        // 诊断文本里必然出现中文：按字节切会踩到非字符边界（真实事故：一次启动期
        // 中文错误在写日志时 panic，进程退出码变成 101）。
        let text =
            "错误：无法建立 capability 根 Authorization: Bearer abc123 路径 /tmp/local-mcp-ws";
        let redacted = redact_for_log(text);
        assert!(!redacted.contains("abc123"), "{redacted}");
        assert!(redacted.contains("无法建立 capability 根"), "{redacted}");
    }

    #[test]
    fn test_redaction_keeps_short_tokens_and_paths_intact() {
        let text = "cargo build --release finished in 3.5s (target/release/local-mcp-server)";
        assert_eq!(redact_for_log(text), text);
    }

    #[test]
    fn test_audit_and_diagnostics_are_safe_to_call_without_subscriber() {
        // 没有安装 subscriber 时 tracing 是 no-op，调用不得 panic（判断路径由
        // 结构化字段决定：这里只要求函数本身不产生副作用）。
        audit_tool_call(&ToolAudit {
            request_id: "req-1",
            principal: "stdio-principal-xxx",
            client_instance: "stdio-conn-xxx",
            tool: "Read",
            outcome: ToolOutcome::Ok,
            elapsed: Duration::from_millis(3),
        });
        // 脱敏入口同样必须可在无 subscriber 时安全调用。
        let redacted = redact_for_log("Authorization: Bearer should-not-appear");
        assert!(!redacted.contains("should-not-appear"));
    }
}
