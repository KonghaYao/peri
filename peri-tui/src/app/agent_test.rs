use peri_acp::event::AcpEvent;

use super::map_acp_event;
use crate::app::AgentEvent;

#[test]
fn test_map_acp_event_execution_failed() {
    let event = AcpEvent::AgentExecutionFailed {
        message: "LLM HTTP 错误 (400)".to_string(),
    };
    let result = map_acp_event(event, "/tmp");
    assert!(result.is_some(), "AgentExecutionFailed should map to Some");
    match result.unwrap() {
        AgentEvent::Error(msg) => {
            assert_eq!(msg, "LLM HTTP 错误 (400)");
        }
        _ => panic!("Expected AgentEvent::Error, got a different variant"),
    }
}

#[test]
fn test_map_acp_event_interrupted() {
    let event = AcpEvent::AgentExecutionFailed {
        message: "Interrupted by user".to_string(),
    };
    let result = map_acp_event(event, "/tmp");
    assert!(
        result.is_some(),
        "AgentExecutionFailed(Interrupted) should map to Some"
    );
    match result.unwrap() {
        AgentEvent::Interrupted => {}
        _ => panic!("Expected AgentEvent::Interrupted, got a different variant"),
    }
}

#[test]
fn test_map_acp_event_context_warning() {
    let event = AcpEvent::ContextWarning {
        used_tokens: 100000,
        total_tokens: 200000,
        percentage: 50.0,
    };
    let result = map_acp_event(event, "/tmp");
    assert!(result.is_some(), "ContextWarning should map to Some");
    match result.unwrap() {
        AgentEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        } => {
            assert_eq!(used_tokens, 100000);
            assert_eq!(total_tokens, 200000);
            assert!((percentage - 50.0).abs() < 0.01);
        }
        _ => panic!("Expected AgentEvent::ContextWarning, got a different variant"),
    }
}

#[test]
fn test_map_acp_event_subagent_lifecycle() {
    let event = AcpEvent::SubagentStarted {
        agent_name: "researcher".to_string(),
        instance_id: "inst_123".to_string(),
        is_background: true,
    };
    let result = map_acp_event(event, "/tmp");
    assert!(result.is_some());
    match result.unwrap() {
        AgentEvent::SubAgentStart {
            agent_id,
            instance_id,
            is_background,
            ..
        } => {
            assert_eq!(agent_id, "researcher");
            assert_eq!(instance_id, "inst_123");
            assert!(is_background);
        }
        _ => panic!("Expected AgentEvent::SubAgentStart"),
    }
}

#[test]
fn test_map_acp_event_llm_retrying() {
    let event = AcpEvent::LlmRetrying {
        attempt: 2,
        max_attempts: 3,
        delay_ms: 1000,
        error: "rate limit".to_string(),
    };
    let result = map_acp_event(event, "/tmp");
    assert!(result.is_some());
    match result.unwrap() {
        AgentEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => {
            assert_eq!(attempt, 2);
            assert_eq!(max_attempts, 3);
            assert_eq!(delay_ms, 1000);
            assert_eq!(error, "rate limit");
        }
        _ => panic!("Expected AgentEvent::LlmRetrying"),
    }
}
