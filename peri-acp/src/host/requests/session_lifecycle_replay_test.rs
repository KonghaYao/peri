use super::TuiReplaySender;
use crate::dispatch::replay_persisted_session_history;
use crate::transport::{mpsc::mpsc_transport_pair, stdio::StdioTransport, AcpTransport};
use peri_acp_types::{messages::BaseMessage, store::PersistedPayload, PeriCaps};
use tokio::io::{AsyncBufReadExt, BufReader};

fn history() -> Vec<PersistedPayload> {
    vec![
        PersistedPayload::Message(BaseMessage::human("真实用户输入")),
        PersistedPayload::Message(BaseMessage::human("[最近读取的文件: /a.rs]\n正文 <&>")),
        PersistedPayload::Message(BaseMessage::human(
            "[激活的 Skill 指令: /a/SKILL.md]\n技能正文",
        )),
    ]
}

fn assert_reminder(params: &serde_json::Value, canonical: bool, body: &str) {
    assert_eq!(params["sessionId"], "replay-test");
    assert_eq!(params["data"]["replay"], true);
    if canonical {
        assert_eq!(params["event"], "system-reminder");
        assert_eq!(params["data"]["reminder"]["category"], "legacy");
        assert_eq!(params["data"]["reminder"]["body"], body);
    } else {
        assert_eq!(params["event"], "system-reminder-fallback");
        assert_eq!(params["data"]["legacy"], true);
        assert_eq!(params["data"]["text"], body);
    }
}

/// [回归测试] TUI 的真实 MPSC 回放出口必须分离用户与 Compact 上下文。
#[tokio::test]
async fn test_compact_reminder_replay_mpsc_wire() {
    for canonical in [true, false] {
        let (client, server) = mpsc_transport_pair();
        let sender = TuiReplaySender { transport: &server };
        let caps = PeriCaps {
            system_reminder: canonical,
            ..Default::default()
        };
        replay_persisted_session_history("replay-test", &history(), &sender, &caps)
            .await
            .unwrap();
        for index in 0..3 {
            let notification =
                tokio::time::timeout(std::time::Duration::from_secs(2), client.recv())
                    .await
                    .unwrap()
                    .unwrap();
            let crate::transport::types::IncomingMessage::Notification { method, params } =
                notification
            else {
                panic!("预期通知");
            };
            if index == 0 {
                assert_eq!(method, "session/update");
                assert_eq!(params["update"]["sessionUpdate"], "user_message_chunk");
            } else {
                assert_eq!(method, "peri/unstable_event");
                assert_reminder(
                    &params,
                    canonical,
                    &history()[index].as_message().unwrap().content(),
                );
            }
        }
    }
}

/// [回归测试] Stdio 使用同一 replay sender，经真实 JSON 行编码保留 reminder 正文。
#[tokio::test]
async fn test_compact_reminder_replay_stdio_wire() {
    for canonical in [true, false] {
        let (input, _input_writer) = tokio::io::duplex(4096);
        let (output, output_reader) = tokio::io::duplex(16384);
        let transport = StdioTransport::from_reader_writer(input, output);
        let sender = TuiReplaySender {
            transport: &transport,
        };
        let caps = PeriCaps {
            system_reminder: canonical,
            ..Default::default()
        };
        replay_persisted_session_history("replay-test", &history(), &sender, &caps)
            .await
            .unwrap();
        let mut lines = BufReader::new(output_reader).lines();
        for index in 0..3 {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let wire: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(wire["jsonrpc"], "2.0");
            if index == 0 {
                assert_eq!(wire["method"], "session/update");
                assert_eq!(wire["params"]["update"]["content"]["text"], "真实用户输入");
            } else {
                assert_eq!(wire["method"], "peri/unstable_event");
                assert_reminder(
                    &wire["params"],
                    canonical,
                    &history()[index].as_message().unwrap().content(),
                );
            }
        }
    }
}
