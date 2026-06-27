use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::interaction::channel_types::ChannelNotification;
use tokio::sync::{mpsc, Notify};

use super::message_pipeline::MessagePipeline;
use crate::ui::{
    message_view::MessageViewModel,
    render_thread::{RenderCache, RenderEvent},
};

/// 消息状态：会话级的消息管线、渲染通道、消息列表。
pub struct MessageState {
    pub view_messages: Vec<MessageViewModel>,
    pub round_start_vm_idx: usize,
    pub pipeline: MessagePipeline,
    pub render_tx: mpsc::Sender<RenderEvent>,
    pub render_cache: Arc<RwLock<RenderCache>>,
    pub render_notify: Arc<Notify>,
    pub last_render_version: u64,
    /// 最近一次提交的用户文本（用于 Ctrl+C 中断时恢复到输入框）
    pub last_submitted_text: Option<String>,
    /// 临时系统通知（不在 BaseMessage[] 中），记录 (锚点索引, VM)。
    /// 锚点 = 创建时 view_messages.len()，RebuildAll 时按锚点插入到对应位置。
    pub ephemeral_notes: Vec<(usize, MessageViewModel)>,
    /// 最近一次发送给渲染线程的 resize 宽度（用于去抖，避免每帧重复发送）
    pub last_resize_width: Option<u16>,
    /// Channel 消息通知接收端
    pub channel_notification_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ChannelNotification>>,
    /// Loading 期间用户缓存的消息（Agent 任务完成后自动提交）。
    ///
    /// 异步事件触发的自动续跑（cron/channel/workflow/bg_results）已由
    /// `polling.rs` 的 v2 queue drain 路径独立处理，与本字段无关 ——
    /// 本字段仅服务用户输入缓存需求。
    pub pending_messages: Vec<String>,
}

impl MessageState {
    pub fn new(
        cwd: String,
        render_tx: mpsc::Sender<RenderEvent>,
        render_cache: Arc<RwLock<RenderCache>>,
        render_notify: Arc<Notify>,
    ) -> Self {
        Self {
            view_messages: Vec::new(),
            round_start_vm_idx: 0,
            pipeline: MessagePipeline::new(cwd),
            render_tx,
            render_cache,
            render_notify,
            last_render_version: 0,
            last_submitted_text: None,
            ephemeral_notes: Vec::new(),
            last_resize_width: None,
            channel_notification_rx: None,
            pending_messages: Vec::new(),
        }
    }

    /// 添加系统通知并记录锚点位置。
    ///
    /// 面板代码（通过 PanelContext）和 App 方法均可调用。
    pub fn push_system_note(&mut self, content: String) {
        let anchor = self.view_messages.len();
        let vm = MessageViewModel::system(content);
        self.ephemeral_notes.push((anchor, vm.clone()));
        self.view_messages.push(vm);
    }
}
