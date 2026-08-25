//! ratatui-kit HitlPopup component.
//!
//! HITL (Human-in-the-Loop) 审批弹窗：从 `HITL_PENDING` atom 读取真实工具调用
//! 信息（tool_name + tool_input + batch），渲染待审批的具体内容。
//!
//! I21-A：替换原 mock_approval() 写死 "FileWrite" 假数据——现在 popup 展示
//! agent 实际触发的工具调用，用户能据此判断该不该授权。
//!
//! ## 用户路径
//!
//! - **Enter**：approve——从 composite 快照取 ID，经 `HITL_RESPONSE_TX` 发 `Approve`
//!   到 hitl_response_consumer，由其按 semantic owner 调用
//!   `client.respond_interaction(owner, selected/allow_once)`。
//! - **Esc**：reject——同样读 id 发 `Reject`（outcome=cancelled）。
//!
//! ## 事件优先级（H2 修复）
//!
//! HitlPopup 的 Enter/Esc handler 用 `EventPriority::High`，先于 `register_root_handlers`
//! 的 Normal Esc handler 执行（root handler 在 `FocusLayer::Popup` 时会直接 `close_popup`
//! 并 Consumed 截断）。High 优先级让 popup 有机会从 composite 快照发响应；
//! Consumed 后 root handler 不再执行，避免 close 在 send 之前清空 id。
//! 其他 popup（AskUser/Rewind/OAuth）仍用 Normal，依赖 root handler 关闭。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::atoms::{HITL_PENDING, HITL_RESPONSE_TX, LANG_VERSION};
use crate::kit::hitl_response::HitlResponseAction;
use crate::kit::panel_mouse::AreaTracker;
use crate::kit::popup_overlay::close_hitl_popup_for_owner;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

#[component]
pub fn HitlPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&HITL_PENDING);
    let pending = pending_store.read().clone();
    let _ = pending_store;

    // 弹窗绘制区域（上一帧）——鼠标整窗点击 = 审批通过
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 审批动作：Enter 与鼠标左键点击共用（click as enter）
    let approve_pending = pending.clone();
    let reject_pending = pending.clone();
    let approve = move || {
        if let Some(snapshot) = &approve_pending
            && let Some(tx) = HITL_RESPONSE_TX.get()
        {
            let _ = tx.send(HitlResponseAction::Approve {
                owner: snapshot.owner.clone(),
                request_id_str: snapshot.request_id_json.clone(),
            });
            close_hitl_popup_for_owner(&snapshot.owner);
        }
    };
    let reject = move || {
        if let Some(snapshot) = &reject_pending
            && let Some(tx) = HITL_RESPONSE_TX.get()
        {
            let _ = tx.send(HitlResponseAction::Reject {
                owner: snapshot.owner.clone(),
                request_id_str: snapshot.request_id_json.clone(),
            });
            close_hitl_popup_for_owner(&snapshot.owner);
        }
    };

    // High 优先级：先于 register_root_handlers 的 Normal Esc handler 执行，
    // 避免 root close_popup 后 Consumed 截断本 handler（详见模块注释「事件优先级」）。
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 执行审批动作（click as enter）
            if let Event::Mouse(mouse) = event {
                if area.is_some() && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    approve();
                    return EventResult::Consumed;
                }
                return EventResult::Ignored;
            }
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    approve();
                    EventResult::Consumed
                }
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    reject();
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            }
        },
    );

    let _ = hooks.use_atom(&LANG_VERSION);

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &pending {
        None => {
            // 理论上不会渲染此分支——POPUP_KIND=Hitl 暗示 HITL_PENDING 已写入
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("popup-hitl-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(pending) => {
            let hp = &pending.payload;
            lines.push(Line::from(""));
            // 工具名行
            lines.push(
                Line::from(i18n::tr_args(
                    "popup-hitl-tool-label",
                    &[("name".to_string(), FluentValue::from(hp.tool_name.as_str()))],
                ))
                .fg(popup_tokens.action_primary)
                .bold(),
            );
            lines.push(Line::from(""));

            // tool_input 字段渲染——序列化为 pretty JSON 后按行展示
            // 限制前 8 个字段避免超长 input 撑爆 popup 高度
            let input_str = serde_json::to_string_pretty(&hp.tool_input)
                .unwrap_or_else(|_| i18n::tr("popup-hitl-non-serializable"));
            let char_count = input_str.chars().count();
            let max_chars = 400;
            let truncated_str: String = input_str.chars().take(max_chars).collect();
            let display_str = if char_count > max_chars {
                format!("{}...", truncated_str)
            } else {
                truncated_str
            };

            for line in display_str.lines().take(8) {
                lines.push(Line::from(format!("    {}", line)).fg(semantic.text.primary));
            }
            if display_str.lines().count() > 8 || char_count > max_chars {
                lines.push(
                    Line::from(i18n::tr_args(
                        "popup-hitl-truncated-info",
                        &[("chars".to_string(), FluentValue::from(char_count as i64))],
                    ))
                    .fg(semantic.text.dim),
                );
            }

            // 批次附加工具（如有）
            if let Some(batch) = &hp.batch
                && !batch.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(
                    Line::from(i18n::tr_args(
                        "popup-hitl-batch-header",
                        &[("more".to_string(), FluentValue::from(batch.len() as i64))],
                    ))
                    .fg(semantic.status.warning),
                );
                for tp in batch.iter().take(4) {
                    lines.push(
                        Line::from(i18n::tr_args(
                            "popup-hitl-batch-item",
                            &[
                                ("name".to_string(), FluentValue::from(tp.tool_name.as_str())),
                                ("input".to_string(), FluentValue::from(tp.tool_id.as_str())),
                            ],
                        ))
                        .fg(semantic.text.muted),
                    );
                }
                if batch.len() > 4 {
                    lines.push(
                        Line::from(i18n::tr_args(
                            "popup-hitl-batch-more",
                            &[(
                                "count".to_string(),
                                FluentValue::from((batch.len() - 4) as i64),
                            )],
                        ))
                        .fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("popup-hitl-action-hint")).fg(semantic.text.dim));
        }
    }

    popup_text_shell!(i18n::tr("popup-hitl-title"), semantic.status.warning, lines)
}
