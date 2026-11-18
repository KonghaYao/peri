//! 提交管线测试（`submit.rs`）：`submit_text` / `dispatch_submit_request`。
//!
//! 与 input_area 的其余测试拆开：本模块覆盖「提交出口」——slash 路由、history、
//! 旧服务端排队缓冲与上传式附件的随行/不丢弃语义。

use super::ACP_STATE;
use super::INPUT_BUFFER;
use super::SubmitRequest;

use std::cell::RefCell;

use crate::app::panel_types::PanelKind;

use peri_tui_components::textarea::TextAreaState;
use ratatui_kit::prelude::{Component, ComponentUpdater, Hooks, Props, State, UseState, element};

use serial_test::serial;

#[must_use]
fn make_submit_recorder() -> std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>> {
    std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()))
}

fn recorded_submit(
    recorder: &std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>>,
) -> Option<SubmitRequest> {
    recorder.lock().pop()
}

#[test]
#[serial]
fn test_submit_text_model_opens_panel_without_history_or_bubble() {
    super::super::reset_submit_side_effect_state();
    super::submit_text("/model".to_string());
    assert_eq!(
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        Some(PanelKind::Model)
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
}

#[test]
#[serial]
fn test_submit_text_clear_sends_session_control_without_history_or_bubble() {
    super::super::reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    super::dispatch_submit_request(crate::kit::submit_request::parse_submit_request("/clear").expect("submit text clear sends session control without history or bubble: the fixture value must satisfy its target representation"), false, |request| {
        recorder.lock().push(request)
    });
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::SessionControl(
            crate::kit::submit_request::SessionControlRequest::Clear,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_provider_sends_view_action_without_history_or_bubble() {
    super::super::reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    super::dispatch_submit_request(
        crate::kit::submit_request::parse_submit_request("/provider").expect("submit text provider sends view action without history or bubble: the fixture value must satisfy its target representation"),
        false,
        |request| recorder.lock().push(request),
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::ViewAction(
            crate::kit::submit_request::ViewActionRequest::CycleProvider,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_compact_appends_bubble_and_history_and_sends_agent_text() {
    super::super::reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    super::dispatch_submit_request(
        crate::kit::submit_request::parse_submit_request("/compact").expect("submit text compact appends bubble and history and sends agent text: the fixture value must satisfy its target representation"),
        false,
        |request| recorder.lock().push(request),
    );
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // UserBubble 通过 LOCAL_EVENT_TX 异步发送，不在此断言
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/compact".to_string(),
            attachments: Vec::new(),
            plugin_selections: Vec::new(),
        })
    );
}

#[test]
#[serial]
fn test_submit_text_unknown_slash_appends_bubble_and_history_and_sends_agent_text() {
    super::super::reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    super::dispatch_submit_request(crate::kit::submit_request::parse_submit_request("/foo").expect("submit text unknown slash appends bubble and history and sends agent text: the fixture value must satisfy its target representation"), false, |request| {
        recorder.lock().push(request)
    });
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments: Vec::new(),
            plugin_selections: Vec::new(),
        })
    );
}

#[test]
#[serial]
fn test_submit_text_loading_unknown_slash_buffers_agent_text() {
    super::super::reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    super::submit_text("/foo".to_string());
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // Slice 3 D4（§10 queued 反转）：loading 提交只入队，**不**发本地气泡——
    // transcript 不得提前出现 user bubble（drain 后才恰一次）。
    assert_eq!(INPUT_BUFFER.state().read().len(), 1);
    assert!(
        VIEW_MODELS.state().read().items.is_empty(),
        "loading 提交不提前进 transcript（排队项显示在 composer 上方队列）"
    );
}

/// Slice 3 D4：排队上限 32 条——超出时队首被挤出（VecDeque FIFO 上限）。
#[test]
#[serial]
fn test_submit_text_loading_queue_caps_at_32() {
    super::super::reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    for i in 0..33 {
        super::submit_text(format!("/c{i}"));
    }
    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 32, "排队上限 32 条");
    assert_eq!(
        buf.front().expect("submit text loading queue caps at 32: the front fixture operation uses valid inputs").text,
        "/c1",
        "超出上限时队首（最旧）被挤出"
    );
    assert_eq!(
        buf.back()
            .expect(
                "submit text loading queue caps at 32: the back fixture operation uses valid inputs"
            )
            .text,
        "/c32"
    );
}

/// Slice 3 D4：非 loading 提交路径不变——本地气泡 + AgentText 双发。
#[test]
#[serial]
fn test_submit_text_not_loading_sends_bubble_and_agent_text() {
    super::super::reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    super::dispatch_submit_request(crate::kit::submit_request::parse_submit_request("/foo").expect("submit text not loading sends bubble and agent text: the fixture value must satisfy its target representation"), false, |request| {
        recorder.lock().push(request)
    });
    assert!(INPUT_BUFFER.state().read().is_empty(), "非 loading 不入队");
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments: Vec::new(),
            plugin_selections: Vec::new(),
        })
    );
    // 本地气泡经 LOCAL_EVENT_TX 异步发送（send_local_user_bubble），
    // transcript 由 acp_bridge 异步写入——本测试不直接断言 VIEW_MODELS
    // （OnceLock 通道不可重置），由 acp_events_test 的 drain 测试覆盖。
}

// ── 上传式附件：提交链上任何一环都不得丢弃 ──────────────────────────────────

fn pending_png() -> crate::kit::atoms::PendingAttachment {
    crate::kit::atoms::PendingAttachment::image("image/png", "AQID")
}

/// loading 排队必须连附件一起排队——排队项只存文本会在 drain 时静默丢图。
#[test]
#[serial]
fn test_submit_text_loading_buffers_attachments_with_text() {
    super::super::reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    super::submit_text("/foo".to_string());

    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 1);
    assert_eq!(buf.front().expect("submit text loading buffers attachments with text: the front fixture operation uses valid inputs").text, "/foo");
    assert_eq!(
        buf.front().expect("submit text loading buffers attachments with text: the front fixture operation uses valid inputs").attachments.len(),
        1,
        "附件随排队项保留"
    );
    assert_eq!(buf.front().expect("submit text loading buffers attachments with text: the front fixture operation uses valid inputs").attachments[0].base64_data, "AQID");
    drop(buf);
    assert!(
        PENDING_ATTACHMENTS.state().read().is_empty(),
        "附件已被排队项接管，不得留在 composer 造成重复提交"
    );
}

/// 非 loading 提交：附件随 `AgentText` 一起下发（上传式，不再经 `@image` 文本）。
#[test]
#[serial]
fn test_submit_text_agent_text_carries_pending_attachments() {
    super::super::reset_submit_side_effect_state();
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    let recorder = make_submit_recorder();
    let request = {
        let attachments = std::mem::take(&mut *PENDING_ATTACHMENTS.state().write());
        SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments,
            plugin_selections: Vec::new(),
        }
    };
    super::dispatch_submit_request(request, false, |request| recorder.lock().push(request));

    match recorded_submit(&recorder) {
        Some(SubmitRequest::AgentText { attachments, .. }) => {
            assert_eq!(attachments.len(), 1, "附件必须随下发请求保留");
            assert_eq!(attachments[0].base64_data, "AQID");
        }
        other => panic!("应下发带附件的 AgentText, got {other:?}"),
    }
}

/// 控制类请求不得消费待发送附件（`/clear` 等不是文本提交）。
#[test]
#[serial]
fn test_submit_text_control_request_keeps_pending_attachments() {
    super::super::reset_submit_side_effect_state();
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    super::submit_text("/clear".to_string());

    assert_eq!(
        PENDING_ATTACHMENTS.state().read().len(),
        1,
        "非 AgentText 提交不得消费附件"
    );
}

/// 用户输入队列可用时，图片-only（空文本 + 附件）提交必须真的入队，
/// 而不是被 parse 的空文本分支丢弃。
#[test]
#[serial]
fn test_submit_text_image_only_enqueues_when_steer_enabled() {
    super::super::reset_submit_side_effect_state();
    crate::kit::steer_state::STEERS.state().write().enabled = true;
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    super::submit_text(String::new());

    assert!(
        PENDING_ATTACHMENTS.state().read().is_empty(),
        "图片-only 提交必须被接管（不得滞留在 composer）"
    );

    // STEER_TX 未初始化 ⇒ 入队命令被拒后进入可恢复待办：附件随原稿可恢复，
    // 不是静默丢失（take-back 经 attachments_from_content 还原附件）。
    let session_id = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let pending = crate::kit::steer_state::STEERS
        .state()
        .read()
        .pending_recovery_ids(&session_id);
    assert_eq!(pending.len(), 1, "失败的入队必须留下可恢复的待办");

    crate::kit::steer_state::STEERS.state().write().enabled = false;
}

/// 待提交插件选择随本条第输入一起冻结：提交后清空，排队项带回原选择。
#[test]
#[serial]
fn test_submit_text_loading_queue_freezes_plugin_selection_with_input() {
    super::super::reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    PENDING_PLUGIN_SELECTIONS.set(vec![peri_acp_types::session::PluginSelection::new("alpha")]);

    super::submit_text("第一个任务".to_string());

    assert!(
        PENDING_PLUGIN_SELECTIONS.state().read().is_empty(),
        "选择随本条输入消费，不能泄漏到下一个任务"
    );
    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 1);
    assert_eq!(
        buf.front()
            .expect("the queued fixture must stay visible")
            .plugin_selections,
        vec![peri_acp_types::session::PluginSelection::new("alpha")],
        "排队序列化必须保留插件选择，drain 时不得静默丢弃"
    );
}

/// 控制类请求不消费待提交插件选择：选择属于下一次 AgentText。
#[test]
#[serial]
fn test_submit_text_control_request_keeps_pending_plugin_selection() {
    super::super::reset_submit_side_effect_state();
    PENDING_PLUGIN_SELECTIONS.set(vec![peri_acp_types::session::PluginSelection::new("alpha")]);

    super::submit_text("/clear".to_string());

    assert_eq!(
        PENDING_PLUGIN_SELECTIONS.state().read().clone(),
        vec![peri_acp_types::session::PluginSelection::new("alpha")],
        "控制类请求不得消费下一次任务的插件选择"
    );
}

// `commit_input` 探针：只有组件 update 里的 `use_state` 能拿到生产同款
// `State<TextAreaState>`，辅助函数测试无法证明真实提交入口没有取走草稿。
type CommitInputVisitor = Box<dyn FnOnce(State<TextAreaState>)>;

thread_local! {
    static COMMIT_INPUT_VISITOR: RefCell<Option<CommitInputVisitor>> = const { RefCell::new(None) };
    static COMMIT_INPUT_DRAFT: RefCell<String> = const { RefCell::new(String::new()) };
}

#[derive(Default, Props)]
struct CommitInputProbeProps;

struct CommitInputProbe;

impl Component for CommitInputProbe {
    type Props<'request> = CommitInputProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        _updater: &mut ComponentUpdater,
    ) {
        let state = hooks.use_state(|| {
            let draft = COMMIT_INPUT_DRAFT.with(|slot| slot.borrow().clone());
            let mut text = TextAreaState::default();
            text.insert_str(&draft);
            text
        });
        COMMIT_INPUT_VISITOR.with(|slot| {
            if let Some(visitor) = slot.borrow_mut().take() {
                visitor(state);
            }
        });
    }
}

/// 在真实 `commit_input` 落点上执行一次探针，返回阻断后的编辑区文本。
fn probe_commit_input(draft: &str) -> String {
    COMMIT_INPUT_DRAFT.with(|slot| *slot.borrow_mut() = draft.to_owned());
    let observed = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
    let sink = observed.clone();
    COMMIT_INPUT_VISITOR.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |state: State<TextAreaState>| {
            super::commit_input(state);
            *sink.lock() = state.read().text.clone();
        }));
    });
    let _ = ratatui_kit::test_util::render_frame(element!(CommitInputProbe()), 1, 1);
    COMMIT_INPUT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    COMMIT_INPUT_DRAFT.with(|slot| slot.borrow_mut().clear());
    observed.lock().clone()
}

/// 未协商 `peri.userInputQueue.pluginSelections` 时，提交必须在取走草稿前阻断：
/// 草稿与选择原样保留，并提示入口不可用（R2/R3）。
#[test]
#[serial]
fn test_commit_input_keeps_draft_and_selection_without_plugin_capability() {
    super::super::reset_submit_side_effect_state();
    let selections = vec![peri_acp_types::session::PluginSelection::new("alpha")];
    crate::kit::atoms::PENDING_PLUGIN_SELECTIONS.set(selections.clone());
    {
        let steers_atom = crate::kit::steer_state::STEERS.state();
        let mut steers = steers_atom.write();
        steers.enabled = true;
        steers.plugin_selections = false;
    }

    let text = probe_commit_input("选择插件后提交");

    assert_eq!(text, "选择插件后提交", "阻断分支不得取走草稿");
    assert_eq!(
        crate::kit::atoms::PENDING_PLUGIN_SELECTIONS
            .state()
            .read()
            .clone(),
        selections,
        "阻断分支不得丢弃插件选择"
    );
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "必须明确提示选择入口不可用"
    );
    assert!(
        crate::kit::atoms::INPUT_BUFFER.state().read().is_empty(),
        "阻断分支不得进入旧服务端排队"
    );

    crate::kit::steer_state::STEERS.state().write().enabled = false;
}

/// 升级后残留的选择不得阻断本地命令：`/setup` 不经过入队协议，
/// 必须正常提交并保留选择给下一次 AgentText。
#[test]
#[serial]
fn test_local_command_ignores_unavailable_plugin_selection_gate() {
    super::super::reset_submit_side_effect_state();
    let selections = vec![peri_acp_types::session::PluginSelection::new("alpha")];
    crate::kit::atoms::PENDING_PLUGIN_SELECTIONS.set(selections.clone());
    {
        let steers_atom = crate::kit::steer_state::STEERS.state();
        let mut steers = steers_atom.write();
        steers.enabled = true;
        steers.plugin_selections = false;
    }

    assert_eq!(
        super::commit_block_reason("/setup"),
        None,
        "本地向导入口不受插件选择能力闸门约束"
    );
    let text = probe_commit_input("/setup");
    assert_eq!(text, "", "本地命令必须正常提交而不是阻断");
    assert_eq!(
        crate::kit::atoms::PENDING_PLUGIN_SELECTIONS
            .state()
            .read()
            .clone(),
        selections,
        "本地命令不得消费下一次任务的插件选择"
    );
    assert!(
        *crate::kit::atoms::WIZARD_ACTIVE.state().read(),
        "本地命令必须执行 ToggleSetup"
    );

    crate::kit::steer_state::STEERS.state().write().enabled = false;
}

/// 图片-only 提交仍属于 AgentText：能力未协商时必须继续阻断。
#[test]
#[serial]
fn test_image_only_submission_still_requires_plugin_capability() {
    super::super::reset_submit_side_effect_state();
    crate::kit::atoms::PENDING_PLUGIN_SELECTIONS
        .set(vec![peri_acp_types::session::PluginSelection::new("alpha")]);
    {
        let steers_atom = crate::kit::steer_state::STEERS.state();
        let mut steers = steers_atom.write();
        steers.enabled = true;
        steers.plugin_selections = false;
    }
    crate::kit::atoms::PENDING_ATTACHMENTS
        .state()
        .write()
        .push(pending_png());

    assert_eq!(
        super::commit_block_reason(""),
        Some(super::CommitBlock::PluginSelectionUnavailable),
        "图片-only 提交同样携带选择，必须 fail-closed"
    );

    crate::kit::atoms::PENDING_ATTACHMENTS
        .state()
        .write()
        .clear();
    crate::kit::steer_state::STEERS.state().write().enabled = false;
}

#[test]
#[serial]
fn test_submit_text_loading_clear_shows_notification_without_history_or_buffer() {
    super::super::reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    super::submit_text("/clear".to_string());
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert!(INPUT_BUFFER.state().read().is_empty());
    assert!(crate::kit::atoms::NOTIFICATION.state().read().is_some());
}

/// 移除一张后再提交：上行内容与输入区呈现同步——只有剩余附件进 content，
/// 已移除的图片不再出现（不残留引用）。
#[test]
#[serial]
fn test_submit_after_removal_sends_remaining_attachments_only() {
    use peri_acp_types::messages::ContentBlock;

    super::super::reset_submit_side_effect_state();
    let atom = PENDING_ATTACHMENTS.state();
    {
        let mut pending = atom.write();
        pending.clear();
        pending.push(crate::kit::atoms::PendingAttachment::image(
            "image/png",
            "AQID",
        ));
        pending.push(crate::kit::atoms::PendingAttachment::image(
            "image/png",
            "BAUG",
        ));
    }

    // 空文本 Backspace：移除尾部一张（图片 2）。
    assert!(super::super::remove_pending_attachment(
        &peri_tui_components::textarea::TextAreaState::default(),
        super::super::AttachmentEdge::Last,
    ));

    let attachments = std::mem::take(&mut *atom.write());
    assert_eq!(attachments.len(), 1, "输入区只剩一张");
    assert_eq!(
        attachments[0].base64_data, "AQID",
        "保留的是未被移除的图片 1"
    );

    let content = crate::kit::steer_state::content_with_attachments("看一下", &attachments);
    let images = content
        .content_blocks()
        .iter()
        .filter(|block| matches!(block, ContentBlock::Image { .. }))
        .count();
    assert_eq!(images, 1, "发给模型的内容只含剩余图片");

    let recorder = make_submit_recorder();
    super::dispatch_submit_request(
        SubmitRequest::AgentText {
            text: "看一下".to_string(),
            attachments,
            plugin_selections: Vec::new(),
        },
        false,
        |request| recorder.lock().push(request),
    );
    match recorded_submit(&recorder) {
        Some(SubmitRequest::AgentText { attachments, .. }) => {
            assert_eq!(attachments.len(), 1, "下发请求不含已移除图片");
        }
        other => panic!("应下发 AgentText, got {other:?}"),
    }
    atom.write().clear();
}

use crate::kit::atoms::PENDING_ATTACHMENTS;
use crate::kit::atoms::PENDING_PLUGIN_SELECTIONS;

use crate::kit::atoms::VIEW_MODELS;
