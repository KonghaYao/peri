use std::sync::Arc;

use async_trait::async_trait;

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
        //
        // [TRAP] Orphan task 行为：首个 broker 响应后，其他 spawned task 继续后台运行
        // 直到完成或 ChannelBroker 的 5 分钟超时（channel_broker.rs:101）。
        // 响应会发送到已无人读取的 mpsc channel，最终随 `tx` drop 而丢弃。
        // ChannelBroker 自身会在成功/超时两条路径清理 pending_permissions（channel_broker.rs:103-107），
        // 因此最坏情况是 5 分钟的"幽灵等待"，无永久资源泄漏。
        // （CS#1 架构审查已确认：影响有限，加注释说明已知行为；未来可加 CancellationToken 提前取消）
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for (name, broker) in &self.brokers {
            let ctx = ctx.clone();
            let broker = broker.clone();
            let name = name.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let response = broker.request(ctx).await;
                let _ = tx.send((name, response));
            });
        }
        // Drop the original sender so rx.recv() returns None when all spawned tasks are done
        drop(tx);

        let (source_name, response) = rx
            .recv()
            .await
            .unwrap_or_else(|| ("error".to_string(), InteractionResponse::Decisions(vec![])));

        // Remaining spawned tasks continue in background; only first responder matters.
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
    }
}
