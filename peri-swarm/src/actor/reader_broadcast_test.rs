use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Wake, Waker};

use super::*;

struct CheckCloseExclusion {
    log: Arc<ItemLog>,
    locked_during_send: AtomicBool,
}

impl Wake for CheckCloseExclusion {
    fn wake(self: Arc<Self>) {
        // broadcast 同步唤醒订阅者；在发送点检查关闭方能否取走 stream。
        self.locked_during_send
            .store(self.log.state.try_lock().is_none(), Ordering::Relaxed);
    }
}

#[tokio::test]
async fn first_live_send_excludes_the_terminal_and_introduces_the_item_once() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    let probe = Arc::new(CheckCloseExclusion {
        log: Arc::clone(&log),
        locked_during_send: AtomicBool::new(false),
    });
    {
        let waker = Waker::from(Arc::clone(&probe));
        let mut recv = std::pin::pin!(events.recv());
        assert!(recv
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        log.ingest("session/update", &chunk("hi")).await;
        assert!(probe.locked_during_send.load(Ordering::Relaxed));
    }
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let frames = official_types(&drain(&mut events));
    assert_eq!(
        &frames[..3],
        [
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.delta",
        ]
    );
    assert_eq!(
        frames
            .iter()
            .filter(|&&kind| kind == "agent.session.turn.item.added")
            .count(),
        1
    );
    assert!(frames[3..].contains(&"agent.session.turn.item.done"));
}

#[tokio::test]
async fn completion_introduces_an_item_whose_live_send_had_no_receivers() {
    let Fixture { log, events, .. } = fixture().await;
    drop(events);
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hi")).await;
    let mut events = log.channel.subscribe();
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let frames = official_types(&drain(&mut events));
    assert_eq!(
        &frames[..2],
        [
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
        ]
    );
    assert!(frames[2..].contains(&"agent.session.turn.item.done"));
}
