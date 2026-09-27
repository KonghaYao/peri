//! `ledger` 的离线测试：唯一键空间、参数化、身份/收据不泄露、解码不猜。全部不联网。

use turso_serverless::Value;

use super::ledger::{
    closure_statement, decode_row, input_digest, qualify_statement, resolve_statement, LedgerRow,
    OperationId, OperationIdentity, OP_LEDGER_TABLE,
};

fn identity(label: &str) -> OperationIdentity {
    OperationIdentity::new(
        OperationId::scoped("run-offline", label),
        "append_history",
        &["thread-1", label],
    )
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

#[test]
fn closure_competes_on_the_same_unique_key_as_qualification() {
    let identity = identity("op-1");
    let qualify = qualify_statement(&identity, "now");
    let closure = closure_statement(&identity, "now");

    // 同一张表的同一主键：另建「closed 表」不构成互斥，这里结构上排除那种写法。
    assert!(qualify.sql.starts_with("INSERT INTO peri_op_ledger"));
    assert!(closure.sql.starts_with("INSERT INTO peri_op_ledger"));
    assert!(qualify.sql.contains(OP_LEDGER_TABLE));
    assert!(closure.sql.contains(OP_LEDGER_TABLE));
    assert!(qualify.sql.contains("'applied'"));
    assert!(closure.sql.contains("'closed'"));
    // 资格写是主键身份，闭合同样带 operation_id（同一唯一键）。
    assert!(qualify
        .params
        .contains(&text(identity.operation_id.as_str())));
    assert!(closure
        .params
        .contains(&text(identity.operation_id.as_str())));
}

#[test]
fn qualification_is_first_statement_shaped_and_parameterized() {
    let identity = identity("op-2");
    let qualify = qualify_statement(&identity, "2026-09-26T00:00:00+00:00");

    for value in [
        identity.operation_id.as_str(),
        identity.kind.as_str(),
        identity.digest.as_str(),
        identity.receipt.as_str(),
    ] {
        assert!(
            !qualify.sql.contains(value),
            "动态内容只能作为绑定参数出现: {value}"
        );
    }
    assert!(qualify.params.contains(&text(identity.receipt.as_str())));
    assert!(qualify.params.contains(&text(identity.digest.as_str())));

    // 解析只读。
    let resolve = resolve_statement(&identity.operation_id);
    assert!(resolve.is_read_only());
    assert!(!resolve.sql.contains(identity.operation_id.as_str()));
    assert!(resolve
        .params
        .contains(&text(identity.operation_id.as_str())));
}

#[test]
fn operation_identity_and_receipt_do_not_debug_leak() {
    let identity = identity("op-3");
    let op_debug = format!("{:?}", identity.operation_id);
    let receipt_debug = format!("{:?}", identity.receipt);
    assert!(!op_debug.contains(identity.operation_id.as_str()));
    assert!(!receipt_debug.contains(identity.receipt.as_str()));
    assert_eq!(op_debug, "OperationId(<opaque>)");
    assert_eq!(receipt_debug, "Receipt(<opaque>)");
}

#[test]
fn digest_is_stable_opaque_and_input_sensitive() {
    let first = input_digest(&["thread-1", "hello"]);
    assert_eq!(first, input_digest(&["thread-1", "hello"]));
    assert_eq!(first.len(), 64);
    assert!(!first.contains("hello") && !first.contains("thread-1"));
    assert_ne!(first, input_digest(&["thread-1", "hellp"]));
    // 长度前缀：拼接歧义必须得到不同摘要。
    assert_ne!(input_digest(&["ab", "c"]), input_digest(&["a", "bc"]));
}

#[test]
fn decoding_never_guesses_a_state() {
    // applied 行必须同时给出收据与摘要：缺一个就无法判断「这是不是同一次操作」。
    match decode_row(&[text("applied"), text("receipt-1"), text("digest-1")]) {
        LedgerRow::Applied { receipt, digest } => {
            assert_eq!(receipt.as_str(), "receipt-1");
            assert_eq!(digest, "digest-1");
        }
        other => panic!("expected applied row, got {other:?}"),
    }
    assert_eq!(
        decode_row(&[text("closed"), Value::Null, Value::Null]),
        LedgerRow::Closed
    );
    assert_eq!(
        decode_row(&[text("applied"), Value::Null, text("digest-1")]),
        LedgerRow::Malformed
    );
    assert_eq!(
        decode_row(&[text("applied"), text("receipt-1"), Value::Null]),
        LedgerRow::Malformed,
        "缺摘要的 applied 行无法做一致性校验，按 Unreadable 处理"
    );
    assert_eq!(
        decode_row(&[text("open"), text("receipt-1"), text("digest-1")]),
        LedgerRow::Malformed
    );
    assert_eq!(
        decode_row(&[Value::Integer(1), text("r"), text("d")]),
        LedgerRow::Malformed
    );
    assert_eq!(decode_row(&[]), LedgerRow::Malformed);
}
