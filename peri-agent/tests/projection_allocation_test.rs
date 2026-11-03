//! Projection must allocate by projected output size, not by canonical body size.
//!
//! The fixture keeps the projected head/tail identical while doubling the source
//! payload. A renderer that copies the whole tool result before truncating will
//! allocate in proportion to the source; the borrowed renderer stays bounded.
//! This counts allocator bytes only (no RSS, no exact allocation-count asserts).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use peri_acp_types::context_archive::build_catalog;
use peri_acp_types::store::PersistedPayload;
use peri_agent::agent::compact_v2::{
    render_llm_view, MessageProjectionDirective, ProjectionAction, ProjectionActionEntry,
    ProjectionTarget, ProviderCapabilities,
};
use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest};
use peri_agent::session::MessageTranscript;

/// Allocator counters are process-global; serialize the measured sections.
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

struct CountingAllocator;

static TRACKING: AtomicBool = AtomicBool::new(false);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct Measured {
    allocated: usize,
    output_len: usize,
}

fn measure(source_chars: usize) -> Measured {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "use a tool",
        vec![ToolCallRequest::new(
            "call-1",
            "Read",
            serde_json::json!({"path": "large.rs"}),
        )],
    ));
    let result_id = transcript.append(BaseMessage::tool_result("call-1", "x".repeat(source_chars)));
    let actions = vec![ProjectionActionEntry {
        message_id: result_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 8,
            keep_tail: 8,
            preserve_recovery_handle: false,
        },
    }];

    ALLOCATED.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);
    let projected =
        render_llm_view(&transcript, &actions, &ProviderCapabilities::openai()).expect("render");
    TRACKING.store(false, Ordering::SeqCst);
    let allocated = ALLOCATED.load(Ordering::SeqCst);
    let output_len = serde_json::to_vec(&projected).expect("serialize").len();

    Measured {
        allocated,
        output_len,
    }
}

#[test]
fn compact_projection_allocates_by_output_not_source_payload() {
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let small = measure(4 * 1024 * 1024);
    let large = measure(8 * 1024 * 1024);

    assert_eq!(
        small.output_len, large.output_len,
        "两个夹具的投影输出尺寸必须一致（sentinel 位数相同）"
    );
    // Head + tail + sentinel + message shell is a few KiB. A per-source-body
    // copy would land at 4 MiB / 8 MiB respectively.
    let bound = 1024 * 1024;
    assert!(
        small.allocated < bound,
        "4 MiB 源正文的投影分配 {} 字节，超过输出级上限 {}",
        small.allocated,
        bound
    );
    assert!(
        large.allocated < bound,
        "8 MiB 源正文的投影分配 {} 字节，超过输出级上限 {}",
        large.allocated,
        bound
    );
    assert!(
        large.allocated.saturating_sub(small.allocated) < 64 * 1024,
        "源正文翻倍只允许常量级分配增长：{} -> {}",
        small.allocated,
        large.allocated
    );
}

fn measure_render(transcript: &MessageTranscript, actions: &[ProjectionActionEntry]) -> Measured {
    ALLOCATED.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);
    let projected =
        render_llm_view(transcript, actions, &ProviderCapabilities::openai()).expect("render");
    TRACKING.store(false, Ordering::SeqCst);
    let allocated = ALLOCATED.load(Ordering::SeqCst);
    let output_len = serde_json::to_vec(&projected).expect("serialize").len();
    Measured {
        allocated,
        output_len,
    }
}

fn omitted_exchange(source_chars: usize) -> Measured {
    let mut transcript = MessageTranscript::new();
    let call = BaseMessage::ai_with_tool_calls(
        "omit this exchange",
        vec![ToolCallRequest::new(
            "call-omit",
            "Read",
            serde_json::json!({"path": "large.rs"}),
        )],
    );
    let result = BaseMessage::tool_result("call-omit", "x".repeat(source_chars));
    let call_id = transcript.append(call.clone());
    let result_id = transcript.append(result.clone());
    // Catalog metadata is bounded separately (64 KiB); use small stand-ins with the
    // same member ids so this test measures renderer body copies only.
    let catalog = build_catalog(
        "session".into(),
        "omit-group".into(),
        call_id,
        &[
            PersistedPayload::Message(BaseMessage::Ai {
                id: call_id,
                content: MessageContent::text("call stand-in"),
                tool_calls: Vec::new(),
            }),
            PersistedPayload::Message(BaseMessage::Tool {
                id: result_id,
                tool_call_id: "call-omit".into(),
                content: MessageContent::text("result stand-in"),
                is_error: false,
                execution: None,
                subagent_failure: None,
            }),
        ],
        "omitted exchange".into(),
    )
    .expect("catalog");
    let action_for = |message_id| ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-omit".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: call_id,
            group_id: catalog.group_id.clone(),
            tool_call_ids: vec!["call-omit".into()],
        },
    };
    let call_action = action_for(call_id);
    let result_action = action_for(result_id);
    transcript.set_flags_projection(
        call_id,
        MessageProjectionDirective {
            policy_version: 3,
            archive_catalog: Some(catalog),
            entries: vec![call_action.clone()],
            ..Default::default()
        },
    );

    measure_render(&transcript, &[call_action, result_action])
}

fn paired_scan(source_chars: usize) -> Measured {
    let arguments = serde_json::json!({"path": "y".repeat(source_chars)});
    let call = BaseMessage::Ai {
        id: peri_agent::messages::MessageId::new(),
        content: MessageContent::Blocks(vec![
            ContentBlock::text("scan this call"),
            ContentBlock::tool_use("call-scan", "Read", arguments.clone()),
        ]),
        tool_calls: vec![ToolCallRequest::new("call-scan", "Read", arguments)],
    };
    let result = BaseMessage::tool_result("call-scan", "ok");
    let mut transcript = MessageTranscript::new();
    let call_id = transcript.append(call.clone());
    let result_id = transcript.append(result.clone());
    let catalog = build_catalog(
        "session".into(),
        "scan-group".into(),
        call_id,
        &[
            PersistedPayload::Message(BaseMessage::Ai {
                id: call_id,
                content: MessageContent::text("call stand-in"),
                tool_calls: Vec::new(),
            }),
            PersistedPayload::Message(BaseMessage::Tool {
                id: result_id,
                tool_call_id: "call-scan".into(),
                content: MessageContent::text("result stand-in"),
                is_error: false,
                execution: None,
                subagent_failure: None,
            }),
        ],
        "scanned exchange".into(),
    )
    .expect("catalog");
    let action_for = |message_id| ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-scan".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: call_id,
            group_id: catalog.group_id.clone(),
            tool_call_ids: vec!["call-scan".into()],
        },
    };
    let call_action = action_for(call_id);
    let result_action = action_for(result_id);
    transcript.set_flags_projection(
        call_id,
        MessageProjectionDirective {
            policy_version: 3,
            archive_catalog: Some(catalog),
            entries: vec![call_action.clone()],
            ..Default::default()
        },
    );

    measure_render(&transcript, &[call_action, result_action])
}

fn assert_source_independent(name: &str, measurements: (Measured, Measured)) {
    let (small, large) = measurements;
    assert_eq!(
        small.output_len, large.output_len,
        "{name}: 两个夹具的投影输出尺寸必须一致"
    );
    let bound = 1024 * 1024;
    assert!(
        small.allocated < bound,
        "{name}: 小源正文的投影分配 {} 字节，超过输出级上限 {bound}",
        small.allocated
    );
    assert!(
        large.allocated < bound,
        "{name}: 大源正文的投影分配 {} 字节，超过输出级上限 {bound}",
        large.allocated
    );
    assert!(
        large.allocated.saturating_sub(small.allocated) < 64 * 1024,
        "{name}: 源正文翻倍只允许常量级分配增长：{} -> {}",
        small.allocated,
        large.allocated
    );
}

#[test]
fn omitted_exchange_does_not_copy_the_source_body() {
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_source_independent(
        "OmitToolExchange",
        (
            omitted_exchange(1024 * 1024),
            omitted_exchange(2 * 1024 * 1024),
        ),
    );
}

#[test]
fn pairing_scan_borrows_canonical_bodies() {
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_source_independent(
        "Pairing",
        (paired_scan(512 * 1024), paired_scan(1024 * 1024)),
    );
}
