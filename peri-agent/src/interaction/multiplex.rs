// [TRAP] ChannelBroker 不支持 Questions 交互类型
// 不应与 TUI broker 参与竞速。
// 详见 spec/global/domains/agent.md#issue_2026-05-29-ask-user-tool-auto-complete
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};

/// 多路 broker：将多个子 broker 的请求竞速，先到先得
pub struct MultiplexBroker {
    brokers: Vec<(String, Arc<dyn UserInteractionBroker>)>,
}

impl MultiplexBroker {
    pub fn new(brokers: Vec<(String, Arc<dyn UserInteractionBroker>)>) -> Self {
        Self { brokers }
    }
}

#[async_trait]
impl UserInteractionBroker for MultiplexBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        if self.brokers.is_empty() {
            return InteractionResponse::Decisions(vec![]);
        }
        if self.brokers.len() == 1 {
            return self.brokers[0].1.request(ctx).await;
        }

        // Spawn all brokers in parallel, race via mpsc channel.
        // 首个响应到达后通过 CancellationToken 提前取消其余 broker。
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        for (name, broker) in &self.brokers {
            let ctx = ctx.clone();
            let broker = broker.clone();
            let name = name.clone();
            let tx = tx.clone();
            let cancel_child = cancel.child_token();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel_child.cancelled() => {
                        // 被 cancel，不发送响应
                    }
                    response = broker.request(ctx) => {
                        let _ = tx.send((name, response));
                    }
                }
            });
        }
        // Drop the original sender so rx.recv() returns None when all spawned tasks are done
        drop(tx);

        let (source_name, response) = rx
            .recv()
            .await
            .unwrap_or_else(|| ("error".to_string(), InteractionResponse::Decisions(vec![])));

        // 收到首个响应后取消其余 broker
        cancel.cancel();
        tag_source(response, &source_name)
    }
}

/// Tag all ApprovalDecision variants with the broker's name
fn tag_source(response: InteractionResponse, source: &str) -> InteractionResponse {
    match response {
        InteractionResponse::Decisions(decisions) => {
            let tagged: Vec<_> = decisions
                .into_iter()
                .map(|d| match d {
                    ApprovalDecision::Approve { .. } => ApprovalDecision::Approve {
                        source: Some(source.to_string()),
                    },
                    ApprovalDecision::Reject { reason, .. } => ApprovalDecision::Reject {
                        reason,
                        source: Some(source.to_string()),
                    },
                    other => other,
                })
                .collect();
            InteractionResponse::Decisions(tagged)
        }
        InteractionResponse::Answers(answers) => InteractionResponse::Answers(answers),
        InteractionResponse::Rejected => InteractionResponse::Rejected,
    }
}
