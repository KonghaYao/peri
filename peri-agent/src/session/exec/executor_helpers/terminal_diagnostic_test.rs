use super::{classify_loop_terminal, log_loop_terminal};
use crate::{agent::stages::LoopResult, error::AgentError};
use std::sync::Arc;

#[derive(Clone, Default)]
struct LogBuffer(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_terminal(result: LoopResult, cancelled: bool) -> String {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let terminal = classify_loop_terminal(&result, cancelled);
        log_loop_terminal(
            "session-diagnostic",
            "turn-diagnostic",
            &result,
            &terminal,
            false,
        );
    });
    let text = String::from_utf8(logs.0.lock().clone()).unwrap();
    text
}

#[test]
fn default_info_fatal_log_retains_chain_and_execution_identity() {
    for root in [
        "database unavailable token=synthetic",
        "receipt revision rejected",
    ] {
        let result = LoopResult::Error(AgentError::Other(
            anyhow::anyhow!(root)
                .context("append transcript")
                .context("finish execution"),
        ));
        let text = capture_terminal(result, false);
        assert_eq!(text.lines().count(), 1);
        for fact in [
            "ERROR",
            "session-diagnostic",
            "turn-diagnostic",
            "other",
            root,
            "append transcript",
            "finish execution",
        ] {
            assert!(text.contains(fact), "missing {fact}: {text}");
        }
        assert!(!text.contains("Check logs for details"));
        assert!(text.find("finish execution").unwrap() < text.find(root).unwrap());
    }
}

#[test]
fn cancelled_and_nonfatal_results_do_not_emit_default_info_errors() {
    let cases = [
        (LoopResult::Interrupted, false),
        (LoopResult::Error(AgentError::Interrupted), false),
        (
            LoopResult::Error(AgentError::Other(anyhow::anyhow!("cancelled late"))),
            true,
        ),
        (
            LoopResult::Error(AgentError::MaxIterationsExceeded(3)),
            false,
        ),
        (
            LoopResult::Error(AgentError::OutputTruncated { attempts: 3 }),
            false,
        ),
        (LoopResult::Completed, true),
    ];
    for (result, cancelled) in cases {
        assert!(capture_terminal(result, cancelled).is_empty());
    }
}

#[test]
fn model_fatal_log_keeps_diagnostics_beyond_public_message_limit() {
    let source =
        peri_model::ModelError::http_status(500, "provider.example", Some("request-diagnostic"))
            .with_message("provider failure")
            .with_body(format!("{} root-at-tail", "x".repeat(2_100)))
            .with_causes(vec!["socket root".to_owned()]);
    let text = capture_terminal(LoopResult::Error(AgentError::ModelError(source)), false);
    for fact in ["500", "root-at-tail", "socket root", "request-diagnostic"] {
        assert!(text.contains(fact), "missing {fact}: {text}");
    }
}
